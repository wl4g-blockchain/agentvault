use std::{fmt, time::Duration};

use async_trait::async_trait;
use redis::{aio::MultiplexedConnection, cluster::ClusterClientBuilder, cluster_async::ClusterConnection};
use tokio::{
    sync::Mutex,
    time::{sleep, timeout},
};
use zeroize::Zeroizing;

use super::port::{effective_ttl, validate_component};
use super::{CacheError, CacheResult, ICache};
use crate::config::CacheProvider;
use crate::config::RedisCacheConfig;
use crate::config::RedisCacheMode;
use crate::config::MAX_CACHE_REDIS_ENDPOINTS;
use crate::config::MAX_CACHE_RETRIES;
use crate::config::MAX_CACHE_TTL;
use crate::config::MAX_CACHE_VALUE_BYTES;
use crate::utils::secret::SecretReader;

const MAX_REDIS_URL_BYTES: usize = 8 * 1024;
const READ_SCRIPT: &str = r"
local raw = redis.call('HGET', KEYS[1], ARGV[1])
if not raw then return false end
local separator = string.find(raw, ':', 1, true)
if not separator then
  redis.call('HDEL', KEYS[1], ARGV[1])
  return false
end
local expires_at = tonumber(string.sub(raw, 1, separator - 1))
local clock = redis.call('TIME')
local now = clock[1] * 1000 + math.floor(clock[2] / 1000)
if not expires_at or expires_at <= now then
  redis.call('HDEL', KEYS[1], ARGV[1])
  return false
end
return string.sub(raw, separator + 1)
";
const SET_SCRIPT: &str = r"
local clock = redis.call('TIME')
local now = clock[1] * 1000 + math.floor(clock[2] / 1000)
redis.call('HSET', KEYS[1], ARGV[1], tostring(now + ARGV[2]) .. ':' .. ARGV[4])
redis.call('PEXPIRE', KEYS[1], ARGV[3])
return 1
";
const SET_IF_ABSENT_SCRIPT: &str = r"
local raw = redis.call('HGET', KEYS[1], ARGV[1])
local clock = redis.call('TIME')
local now = clock[1] * 1000 + math.floor(clock[2] / 1000)
if raw then
  local separator = string.find(raw, ':', 1, true)
  local expires_at = separator and tonumber(string.sub(raw, 1, separator - 1)) or nil
  if expires_at and expires_at > now then return 0 end
end
redis.call('HSET', KEYS[1], ARGV[1], tostring(now + ARGV[2]) .. ':' .. ARGV[4])
redis.call('PEXPIRE', KEYS[1], ARGV[3])
return 1
";
const TAKE_SCRIPT: &str = r"
local raw = redis.call('HGET', KEYS[1], ARGV[1])
if not raw then return false end
redis.call('HDEL', KEYS[1], ARGV[1])
local separator = string.find(raw, ':', 1, true)
if not separator then return false end
local expires_at = tonumber(string.sub(raw, 1, separator - 1))
local clock = redis.call('TIME')
local now = clock[1] * 1000 + math.floor(clock[2] / 1000)
if not expires_at or expires_at <= now then return false end
return string.sub(raw, separator + 1)
";

#[derive(Clone)]
enum RedisConnection {
    Standalone(MultiplexedConnection),
    Cluster(ClusterConnection),
}

enum RedisLifecycle {
    Created,
    Ready(RedisConnection),
    Closed,
}

/// Redis-backed cache supporting a multiplexed standalone connection or an
/// asynchronous Redis Cluster connection. Redis types do not cross the cache
/// port boundary.
pub struct RedisCache {
    state: Mutex<RedisLifecycle>,
    config: RedisCacheConfig,
}

impl RedisCache {
    /// Builds an uninitialized Redis provider from validated configuration.
    /// Network I/O is performed exclusively by [`ICache::init`], allowing the
    /// factory to register the instance before initialization and roll it back
    /// atomically when initialization fails.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid bounds or provider configuration.
    pub(crate) fn from_config(config: &RedisCacheConfig) -> CacheResult<Self> {
        validate_config(config)?;
        Ok(Self {
            state: Mutex::new(RedisLifecycle::Created),
            config: config.clone(),
        })
    }

    fn namespace_key(&self, namespace: &str) -> CacheResult<String> {
        namespace_key(&self.config.key_prefix, namespace)
    }

    fn field_name(key: &str) -> CacheResult<&str> {
        validate_component(key)?;
        Ok(key)
    }

    async fn execute<T>(&self, command: redis::Cmd) -> CacheResult<T>
    where
        T: redis::FromRedisValue + Send,
    {
        let connection = {
            let state = self.state.lock().await;
            match &*state {
                RedisLifecycle::Ready(connection) => connection.clone(),
                RedisLifecycle::Created => {
                    return Err(CacheError::Unavailable("Redis cache is not initialized".to_owned()));
                }
                RedisLifecycle::Closed => {
                    return Err(CacheError::Unavailable("Redis cache has been closed".to_owned()));
                }
            }
        };
        let operation = async {
            match connection {
                RedisConnection::Standalone(connection) => {
                    let mut connection = connection;
                    command.query_async::<T>(&mut connection).await
                }
                RedisConnection::Cluster(connection) => {
                    let mut connection = connection;
                    command.query_async::<T>(&mut connection).await
                }
            }
        };
        timeout(self.config.operation_timeout, operation)
            .await
            .map_err(|_| CacheError::Unavailable("Redis operation timed out".to_owned()))?
            .map_err(|error| redis_error("Redis operation failed", &error))
    }
}

impl fmt::Debug for RedisCache {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RedisCache")
            .field("state", &"<redacted>")
            .field("key_prefix", &self.config.key_prefix)
            .field("max_value_bytes", &self.config.max_value_bytes)
            .field("operation_timeout", &self.config.operation_timeout)
            .field("default_ttl", &self.config.default_ttl)
            .finish()
    }
}

#[async_trait]
impl ICache for RedisCache {
    fn provider(&self) -> CacheProvider {
        CacheProvider::Redis
    }

    async fn init(&self) -> CacheResult<()> {
        let mut state = self.state.lock().await;
        match &*state {
            RedisLifecycle::Ready(_) => return Ok(()),
            RedisLifecycle::Closed => {
                return Err(CacheError::Unavailable("Redis cache has been closed".to_owned()));
            }
            RedisLifecycle::Created => {}
        }

        let urls = resolve_urls(&self.config)?;
        let mut attempt = 0;
        let failure = loop {
            match connect_once(&self.config, &urls).await {
                Ok(connection) => {
                    *state = RedisLifecycle::Ready(connection);
                    return Ok(());
                }
                Err(failure) if attempt >= self.config.retry_attempts => break failure,
                Err(_) => {
                    let delay = retry_delay(self.config.min_retry_wait, self.config.max_retry_wait, attempt);
                    attempt = attempt.saturating_add(1);
                    tracing::warn!(
                        attempt,
                        max_attempts = self.config.retry_attempts + 1,
                        retry_delay_ms = delay.as_millis(),
                        "Redis cache is unavailable; retrying initial connection"
                    );
                    sleep(delay).await;
                }
            }
        };
        Err(CacheError::Unavailable(failure.to_owned()))
    }

    async fn get(&self, namespace: &str, key: &str) -> CacheResult<Option<Vec<u8>>> {
        let mut command = redis::cmd("EVAL");
        command
            .arg(READ_SCRIPT)
            .arg(1_u8)
            .arg(self.namespace_key(namespace)?)
            .arg(Self::field_name(key)?);
        self.execute(command).await
    }

    async fn set(&self, namespace: &str, key: &str, value: Vec<u8>, ttl: Duration) -> CacheResult<()> {
        if value.len() > self.config.max_value_bytes {
            return Err(CacheError::InvalidValue(
                "cache value exceeds the configured limit".to_owned(),
            ));
        }
        let ttl = effective_ttl(ttl, self.config.default_ttl)?;
        let ttl_millis = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX).max(1);
        let namespace_ttl_millis = duration_millis(self.config.default_ttl);
        let mut command = redis::cmd("EVAL");
        command
            .arg(SET_SCRIPT)
            .arg(1_u8)
            .arg(self.namespace_key(namespace)?)
            .arg(Self::field_name(key)?)
            .arg(ttl_millis)
            .arg(namespace_ttl_millis)
            .arg(value);
        let _: u8 = self.execute(command).await?;
        Ok(())
    }

    async fn set_if_absent(&self, namespace: &str, key: &str, value: Vec<u8>, ttl: Duration) -> CacheResult<bool> {
        if value.len() > self.config.max_value_bytes {
            return Err(CacheError::InvalidValue(
                "cache value exceeds the configured limit".to_owned(),
            ));
        }
        let ttl = effective_ttl(ttl, self.config.default_ttl)?;
        let ttl_millis = u64::try_from(ttl.as_millis()).unwrap_or(u64::MAX).max(1);
        let namespace_ttl_millis = duration_millis(self.config.default_ttl);
        let mut command = redis::cmd("EVAL");
        command
            .arg(SET_IF_ABSENT_SCRIPT)
            .arg(1_u8)
            .arg(self.namespace_key(namespace)?)
            .arg(Self::field_name(key)?)
            .arg(ttl_millis)
            .arg(namespace_ttl_millis)
            .arg(value);
        let inserted: u8 = self.execute(command).await?;
        Ok(inserted == 1)
    }

    async fn take(&self, namespace: &str, key: &str) -> CacheResult<Option<Vec<u8>>> {
        let mut command = redis::cmd("EVAL");
        command
            .arg(TAKE_SCRIPT)
            .arg(1_u8)
            .arg(self.namespace_key(namespace)?)
            .arg(Self::field_name(key)?);
        self.execute(command).await
    }

    async fn delete(&self, namespace: &str, key: &str) -> CacheResult<()> {
        let mut command = redis::cmd("HDEL");
        command.arg(self.namespace_key(namespace)?).arg(Self::field_name(key)?);
        let _: u64 = self.execute(command).await?;
        Ok(())
    }

    async fn exists(&self, namespace: &str, key: &str) -> CacheResult<bool> {
        Ok(self.get(namespace, key).await?.is_some())
    }

    async fn clear(&self, namespace: &str) -> CacheResult<()> {
        let mut command = redis::cmd("DEL");
        command.arg(self.namespace_key(namespace)?);
        let _: u64 = self.execute(command).await?;
        Ok(())
    }

    async fn close(&self) -> CacheResult<()> {
        let mut state = self.state.lock().await;
        *state = RedisLifecycle::Closed;
        Ok(())
    }

    async fn ping(&self) -> CacheResult<()> {
        let response: String = self.execute(redis::cmd("PING")).await?;
        if response == "PONG" {
            Ok(())
        } else {
            Err(CacheError::Unavailable(
                "Redis returned an invalid PING response".to_owned(),
            ))
        }
    }
}

fn validate_config(config: &RedisCacheConfig) -> CacheResult<()> {
    if config.urls.is_empty()
        || (config.mode == RedisCacheMode::Standalone && config.urls.len() != 1)
        || config.urls.len() > MAX_CACHE_REDIS_ENDPOINTS
        || config.key_prefix.is_empty()
        || config.key_prefix.len() > 128
        || config.key_prefix.starts_with(':')
        || config.key_prefix.ends_with(':')
        || !config
            .key_prefix
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'-'))
        || config.max_value_bytes == 0
        || config.max_value_bytes > MAX_CACHE_VALUE_BYTES
        || config.connect_timeout.is_zero()
        || config.operation_timeout.is_zero()
        || config.default_ttl.is_zero()
        || config.default_ttl > MAX_CACHE_TTL
        || config.retry_attempts > MAX_CACHE_RETRIES
        || config.min_retry_wait.is_zero()
        || config.max_retry_wait < config.min_retry_wait
    {
        return Err(CacheError::InvalidValue(
            "Redis cache configuration is invalid".to_owned(),
        ));
    }
    Ok(())
}

fn resolve_urls(config: &RedisCacheConfig) -> CacheResult<Vec<Zeroizing<String>>> {
    config
        .urls
        .iter()
        .map(|reference| {
            if !reference.file.is_empty() || reference.env.is_empty() {
                return Err(CacheError::InvalidValue(
                    "Redis URLs must use environment-backed secret references".to_owned(),
                ));
            }
            SecretReader::read_ref(reference, "Redis cache URL", MAX_REDIS_URL_BYTES)
                .map_err(|_| CacheError::Unavailable("Redis cache URL environment variable is unavailable".to_owned()))
        })
        .collect()
}

async fn connect_once(config: &RedisCacheConfig, urls: &[Zeroizing<String>]) -> Result<RedisConnection, &'static str> {
    let connect = async {
        match config.mode {
            RedisCacheMode::Standalone => {
                let client = redis::Client::open(urls[0].as_str()).map_err(|_| "Redis standalone URL is invalid")?;
                let mut connection = client
                    .get_multiplexed_async_connection()
                    .await
                    .map_err(|_| "Redis standalone connection failed")?;
                let response = redis::cmd("PING")
                    .query_async::<String>(&mut connection)
                    .await
                    .map_err(|_| "Redis standalone PING failed")?;
                if response != "PONG" {
                    return Err("Redis standalone returned an invalid PING response");
                }
                Ok(RedisConnection::Standalone(connection))
            }
            RedisCacheMode::Cluster => {
                let nodes = urls.iter().map(|url| url.to_string()).collect::<Vec<_>>();
                let client = ClusterClientBuilder::new(nodes)
                    .connection_timeout(config.connect_timeout)
                    .response_timeout(config.operation_timeout)
                    .retries(config.retry_attempts)
                    .min_retry_wait(duration_millis(config.min_retry_wait))
                    .max_retry_wait(duration_millis(config.max_retry_wait))
                    .build()
                    .map_err(|_| "Redis Cluster URL or retry configuration is invalid")?;
                let mut connection = client
                    .get_async_connection()
                    .await
                    .map_err(|_| "Redis Cluster connection failed")?;
                let response = redis::cmd("PING")
                    .query_async::<String>(&mut connection)
                    .await
                    .map_err(|_| "Redis Cluster PING failed")?;
                if response != "PONG" {
                    return Err("Redis Cluster returned an invalid PING response");
                }
                Ok(RedisConnection::Cluster(connection))
            }
        }
    };
    timeout(config.connect_timeout, connect)
        .await
        .map_err(|_| "Redis connection timed out")?
}

fn namespace_key(prefix: &str, namespace: &str) -> CacheResult<String> {
    validate_component(namespace)?;
    // One hash per namespace makes every operation single-slot in Redis
    // Cluster. The hash tag is derived only from a validated component.
    Ok(format!("{prefix}:{{{namespace}}}:entries"))
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn retry_delay(minimum: Duration, maximum: Duration, attempt: u32) -> Duration {
    minimum
        .saturating_mul(1_u32.checked_shl(attempt.min(31)).unwrap_or(u32::MAX))
        .min(maximum)
}

fn redis_error(context: &str, error: &redis::RedisError) -> CacheError {
    CacheError::Unavailable(format!("{context} ({:?})", error.kind()))
}

#[cfg(test)]
#[path = "../../tests/cache/redis_test.rs"]
mod tests;
