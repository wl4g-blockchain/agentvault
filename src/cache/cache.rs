//! Shared contract and factory for reconstructible or safely disposable
//! short-lived coordination data.
//!
//! Cache entries are never authoritative for budgets, grants, settlement,
//! idempotency, or ledger facts. Both adapters implement identical bounded
//! namespace semantics; `clear` can only remove one validated namespace.

use std::{sync::Arc, time::Duration};

use crate::config::{CacheConfig, CacheProvider};
use async_trait::async_trait;

use super::{MemoryCache, RedisCache};

#[derive(Debug, thiserror::Error)]
pub enum CacheError {
    #[error("cache backend unavailable: {0}")]
    Unavailable(String),
    #[error("cache value is invalid: {0}")]
    InvalidValue(String),
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

/// Sole composition boundary for non-authoritative cache providers.
#[derive(Default)]
pub struct CacheFactory;

impl CacheFactory {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Selects, initializes, and returns the configured process-wide cache.
    ///
    /// # Errors
    ///
    /// Returns an error when provider configuration is invalid or the selected
    /// provider cannot initialize.
    pub async fn create(&self, config: &CacheConfig) -> CacheResult<Arc<dyn ICache>> {
        let component: Arc<dyn ICache> = match config.provider {
            CacheProvider::Memory => Arc::new(MemoryCache::from_config(&config.memory)?),
            CacheProvider::Redis => Arc::new(RedisCache::from_config(&config.redis)?),
        };
        if let Err(error) = component.init().await {
            let _ = component.close().await;
            return Err(error);
        }
        Ok(component)
    }
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
