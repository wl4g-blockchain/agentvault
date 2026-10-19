use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use async_trait::async_trait;
use tokio::sync::Mutex;

use super::port::{effective_ttl, validate_component};
use super::{CacheError, CacheResult, ICache};
use crate::config::{CacheProvider, MemoryCacheConfig, MAX_CACHE_ENTRIES, MAX_CACHE_TTL, MAX_CACHE_VALUE_BYTES};

struct Entry {
    value: Vec<u8>,
    expires_at: Instant,
    last_access_sequence: u64,
}

struct State {
    entries: HashMap<(String, String), Entry>,
    next_access_sequence: u64,
}

/// Process-local bounded LRU cache. Restart safety comes from authoritative
/// storage, never from this map.
pub struct MemoryCache {
    state: Mutex<State>,
    initialized: AtomicBool,
    closed: AtomicBool,
    max_entries: usize,
    max_value_bytes: usize,
    default_ttl: Duration,
}

impl MemoryCache {
    /// Creates a bounded cache with the same value and TTL semantics as Redis.
    ///
    /// # Errors
    ///
    /// Returns an error when an entry, value, or TTL limit is outside the
    /// supported safety bounds.
    pub fn new(max_entries: usize, max_value_bytes: usize, default_ttl: Duration) -> CacheResult<Self> {
        if max_entries == 0
            || max_entries > MAX_CACHE_ENTRIES
            || max_value_bytes == 0
            || max_value_bytes > MAX_CACHE_VALUE_BYTES
            || default_ttl.is_zero()
            || default_ttl > MAX_CACHE_TTL
        {
            return Err(CacheError::InvalidValue(
                "cache bounds or TTL are outside the supported range".to_owned(),
            ));
        }
        Ok(Self {
            state: Mutex::new(State {
                entries: HashMap::with_capacity(max_entries.min(1024)),
                next_access_sequence: 0,
            }),
            initialized: AtomicBool::new(false),
            closed: AtomicBool::new(false),
            max_entries,
            max_value_bytes,
            default_ttl,
        })
    }

    pub(crate) fn from_config(config: &MemoryCacheConfig) -> CacheResult<Self> {
        Self::new(config.max_entries, config.max_value_bytes, config.default_ttl)
    }

    fn validate_value(&self, value: &[u8]) -> CacheResult<()> {
        if value.len() > self.max_value_bytes {
            return Err(CacheError::InvalidValue(
                "cache value exceeds the configured limit".to_owned(),
            ));
        }
        Ok(())
    }

    fn ensure_ready(&self) -> CacheResult<()> {
        if self.closed.load(Ordering::Acquire) || !self.initialized.load(Ordering::Acquire) {
            return Err(CacheError::Unavailable(
                "memory cache is not initialized or has been closed".to_owned(),
            ));
        }
        Ok(())
    }

    fn remove_expired(state: &mut State, now: Instant) {
        state.entries.retain(|_, entry| entry.expires_at > now);
    }

    fn make_room(&self, state: &mut State, cache_key: &(String, String)) {
        if state.entries.len() < self.max_entries || state.entries.contains_key(cache_key) {
            return;
        }
        let least_recently_used = state
            .entries
            .iter()
            .min_by(|(left_key, left), (right_key, right)| {
                left.last_access_sequence
                    .cmp(&right.last_access_sequence)
                    .then_with(|| left_key.cmp(right_key))
            })
            .map(|(entry_key, _)| entry_key.clone());
        if let Some(entry_key) = least_recently_used {
            state.entries.remove(&entry_key);
        }
    }

    fn insert(&self, state: &mut State, cache_key: (String, String), value: Vec<u8>, expires_at: Instant) {
        self.make_room(state, &cache_key);
        state.next_access_sequence = state.next_access_sequence.saturating_add(1);
        let access_sequence = state.next_access_sequence;
        state.entries.insert(
            cache_key,
            Entry {
                value,
                expires_at,
                last_access_sequence: access_sequence,
            },
        );
    }
}

#[async_trait]
impl ICache for MemoryCache {
    fn provider(&self) -> CacheProvider {
        CacheProvider::Memory
    }

    async fn init(&self) -> CacheResult<()> {
        if self.closed.load(Ordering::Acquire) {
            return Err(CacheError::Unavailable("memory cache has been closed".to_owned()));
        }
        self.initialized.store(true, Ordering::Release);
        Ok(())
    }

    async fn get(&self, namespace: &str, key: &str) -> CacheResult<Option<Vec<u8>>> {
        self.ensure_ready()?;
        validate_component(namespace)?;
        validate_component(key)?;
        let cache_key = (namespace.to_owned(), key.to_owned());
        let now = Instant::now();
        let mut state = self.state.lock().await;
        Self::remove_expired(&mut state, now);
        state.next_access_sequence = state.next_access_sequence.saturating_add(1);
        let access_sequence = state.next_access_sequence;
        Ok(state.entries.get_mut(&cache_key).map(|entry| {
            entry.last_access_sequence = access_sequence;
            entry.value.clone()
        }))
    }

    async fn set(&self, namespace: &str, key: &str, value: Vec<u8>, ttl: Duration) -> CacheResult<()> {
        self.ensure_ready()?;
        validate_component(namespace)?;
        validate_component(key)?;
        self.validate_value(&value)?;
        let ttl = effective_ttl(ttl, self.default_ttl)?;
        let now = Instant::now();
        let cache_key = (namespace.to_owned(), key.to_owned());
        let mut state = self.state.lock().await;
        Self::remove_expired(&mut state, now);
        self.insert(&mut state, cache_key, value, now + ttl);
        Ok(())
    }

    async fn set_if_absent(&self, namespace: &str, key: &str, value: Vec<u8>, ttl: Duration) -> CacheResult<bool> {
        self.ensure_ready()?;
        validate_component(namespace)?;
        validate_component(key)?;
        self.validate_value(&value)?;
        let ttl = effective_ttl(ttl, self.default_ttl)?;
        let now = Instant::now();
        let cache_key = (namespace.to_owned(), key.to_owned());
        let mut state = self.state.lock().await;
        Self::remove_expired(&mut state, now);
        if state.entries.contains_key(&cache_key) {
            return Ok(false);
        }
        self.insert(&mut state, cache_key, value, now + ttl);
        Ok(true)
    }

    async fn take(&self, namespace: &str, key: &str) -> CacheResult<Option<Vec<u8>>> {
        self.ensure_ready()?;
        validate_component(namespace)?;
        validate_component(key)?;
        let now = Instant::now();
        let mut state = self.state.lock().await;
        Self::remove_expired(&mut state, now);
        Ok(state
            .entries
            .remove(&(namespace.to_owned(), key.to_owned()))
            .map(|entry| entry.value))
    }

    async fn delete(&self, namespace: &str, key: &str) -> CacheResult<()> {
        self.ensure_ready()?;
        validate_component(namespace)?;
        validate_component(key)?;
        self.state
            .lock()
            .await
            .entries
            .remove(&(namespace.to_owned(), key.to_owned()));
        Ok(())
    }

    async fn exists(&self, namespace: &str, key: &str) -> CacheResult<bool> {
        self.ensure_ready()?;
        Ok(self.get(namespace, key).await?.is_some())
    }

    async fn clear(&self, namespace: &str) -> CacheResult<()> {
        self.ensure_ready()?;
        validate_component(namespace)?;
        self.state
            .lock()
            .await
            .entries
            .retain(|(entry_namespace, _), _| entry_namespace != namespace);
        Ok(())
    }

    async fn close(&self) -> CacheResult<()> {
        if self.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.initialized.store(false, Ordering::Release);
        self.state.lock().await.entries.clear();
        Ok(())
    }

    async fn ping(&self) -> CacheResult<()> {
        self.ensure_ready()?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/cache/memory_test.rs"]
mod tests;
