//! Shared contract and factory for reconstructible cache data.
//!
//! Cache entries are never authoritative for budgets, grants, settlement,
//! idempotency, or ledger facts. Both adapters implement identical bounded
//! namespace semantics; `clear` can only remove one validated namespace.

use std::{collections::BTreeMap, sync::Arc, time::Duration};

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::config::{CacheConfig, CacheProvider};

use super::{MemoryCache, RedisCache};

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache backend unavailable: {0}")]
    Unavailable(String),
    #[error("cache value is invalid: {0}")]
    InvalidValue(String),
    #[error("cache instance conflicts with its registered configuration: {0}")]
    Conflict(String),
    #[error("cache instance was not found: {0}")]
    NotFound(String),
}

pub type CacheResult<T> = std::result::Result<T, CacheError>;

/// Common non-authoritative cache contract.
#[async_trait]
pub trait ICache: Send + Sync {
    /// Identifies the concrete provider selected by the factory.
    fn provider(&self) -> CacheProvider;

    /// Initializes provider-owned resources. Implementations must be
    /// idempotent while open and must reject reinitialization after `close`.
    async fn init(&self) -> CacheResult<()>;

    async fn get(&self, namespace: &str, key: &str) -> CacheResult<Option<Vec<u8>>>;

    async fn set(&self, namespace: &str, key: &str, value: Vec<u8>, ttl: Duration) -> CacheResult<()>;

    async fn delete(&self, namespace: &str, key: &str) -> CacheResult<()>;

    async fn exists(&self, namespace: &str, key: &str) -> CacheResult<bool>;

    /// Removes only entries within one validated logical namespace.
    async fn clear(&self, namespace: &str) -> CacheResult<()>;

    /// Releases adapter-owned resources. Implementations must be idempotent.
    async fn close(&self) -> CacheResult<()>;

    /// Stores a value only when no unexpired value exists for the key.
    ///
    /// This is suitable for best-effort coalescing only. Financial
    /// idempotency and reservations must use authoritative SQL storage.
    async fn set_if_absent(&self, namespace: &str, key: &str, value: Vec<u8>, ttl: Duration) -> CacheResult<bool>;

    /// Atomically returns and removes a value. Approval grants and budget
    /// reservations must never use this non-authoritative primitive.
    async fn take(&self, namespace: &str, key: &str) -> CacheResult<Option<Vec<u8>>>;

    async fn ping(&self) -> CacheResult<()>;
}

struct RegisteredCache {
    config: CacheConfig,
    component: Arc<dyn ICache>,
}

/// Sole composition boundary for non-authoritative cache providers.
///
/// Instances are keyed by `CacheConfig::instance_key`. Recreating a key with
/// identical configuration returns the same initialized component; changing
/// any configuration under an existing key fails closed. Providers are
/// registered before `init`, and an initialization failure removes and closes
/// the failed component before returning.
#[derive(Default)]
pub struct CacheFactory {
    registry: Mutex<BTreeMap<String, RegisteredCache>>,
}

impl CacheFactory {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Selects, registers, initializes, and returns one configured cache.
    ///
    /// # Errors
    ///
    /// Returns an error when the instance key or provider configuration is
    /// invalid, the key is already bound to different configuration, or the
    /// selected provider cannot initialize.
    pub async fn create(&self, config: &CacheConfig) -> CacheResult<Arc<dyn ICache>> {
        validate_instance_key(&config.instance_key)?;
        let mut registry = self.registry.lock().await;
        if let Some(registered) = registry.get(&config.instance_key) {
            return if registered.config == *config {
                Ok(Arc::clone(&registered.component))
            } else {
                Err(CacheError::Conflict(config.instance_key.clone()))
            };
        }

        let component: Arc<dyn ICache> = match config.provider {
            CacheProvider::Memory => Arc::new(MemoryCache::from_config(&config.memory)?),
            CacheProvider::Redis => Arc::new(RedisCache::from_config(&config.redis)?),
        };
        registry.insert(
            config.instance_key.clone(),
            RegisteredCache {
                config: config.clone(),
                component: Arc::clone(&component),
            },
        );

        if let Err(error) = component.init().await {
            registry.remove(&config.instance_key);
            drop(registry);
            let _ = component.close().await;
            return Err(error);
        }
        Ok(component)
    }

    /// Returns a previously initialized cache by its stable instance key.
    ///
    /// # Errors
    ///
    /// Returns an error when the key is invalid or no initialized cache is
    /// registered under it.
    pub async fn get(&self, instance_key: &str) -> CacheResult<Arc<dyn ICache>> {
        validate_instance_key(instance_key)?;
        self.registry
            .lock()
            .await
            .get(instance_key)
            .map(|registered| Arc::clone(&registered.component))
            .ok_or_else(|| CacheError::NotFound(instance_key.to_owned()))
    }

    /// Unregisters one component before closing it. Closing a missing key is an
    /// idempotent success.
    ///
    /// # Errors
    ///
    /// Returns an error when the key is invalid or the registered provider
    /// cannot close cleanly.
    pub async fn close(&self, instance_key: &str) -> CacheResult<()> {
        validate_instance_key(instance_key)?;
        let component = self
            .registry
            .lock()
            .await
            .remove(instance_key)
            .map(|registered| registered.component);
        if let Some(component) = component {
            component.close().await?;
        }
        Ok(())
    }

    /// Unregisters every component first, then closes all providers. Every
    /// close is attempted and the first failure is returned.
    ///
    /// # Errors
    ///
    /// Returns the first provider shutdown failure after attempting to close
    /// every registered cache.
    pub async fn shutdown_all(&self) -> CacheResult<()> {
        let components = {
            let mut registry = self.registry.lock().await;
            std::mem::take(&mut *registry)
                .into_values()
                .map(|registered| registered.component)
                .collect::<Vec<_>>()
        };
        let mut first_error = None;
        for component in components {
            if let Err(error) = component.close().await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

fn validate_instance_key(value: &str) -> CacheResult<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(CacheError::InvalidValue("cache instance key is invalid".to_owned()));
    }
    Ok(())
}

pub(super) fn validate_component(value: &str) -> CacheResult<()> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(CacheError::InvalidValue("cache key component is unsafe".to_owned()));
    }
    Ok(())
}

pub(super) fn effective_ttl(requested: Duration, maximum: Duration) -> CacheResult<Duration> {
    if requested.is_zero() || maximum.is_zero() {
        return Err(CacheError::InvalidValue("cache TTL must be positive".to_owned()));
    }
    Ok(requested.min(maximum))
}

#[cfg(test)]
#[path = "../../tests/cache/cache_test.rs"]
mod tests;
