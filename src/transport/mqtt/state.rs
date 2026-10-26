//! Bounded concurrency and ephemeral MQ reply deduplication.
use super::*;
pub(crate) struct ConcurrencyPermits {
    pub(crate) _global: OwnedSemaphorePermit,
    pub(crate) _client: OwnedSemaphorePermit,
}

pub(crate) struct ConcurrencyLimiter {
    pub(crate) global: Arc<Semaphore>,
    pub(crate) clients: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl ConcurrencyLimiter {
    pub(crate) fn new() -> Self {
        Self {
            global: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            clients: Mutex::new(HashMap::new()),
        }
    }

    pub(crate) async fn try_acquire(&self, client_id: &str) -> Option<ConcurrencyPermits> {
        let global = Arc::clone(&self.global).try_acquire_owned().ok()?;
        let client_limiter = {
            let mut clients = self.clients.lock().await;
            Arc::clone(
                clients
                    .entry(client_id.to_owned())
                    .or_insert_with(|| Arc::new(Semaphore::new(MAX_IN_FLIGHT_PER_CLIENT))),
            )
        };
        let client = client_limiter.try_acquire_owned().ok()?;
        Some(ConcurrencyPermits {
            _global: global,
            _client: client,
        })
    }
}

pub(crate) enum CacheDisposition {
    New,
    InFlight,
    Cached(ResponseEnvelope),
    Conflict,
}

pub(crate) struct ResponseCache {
    pub(crate) cache: Arc<dyn ICache>,
}

#[derive(Deserialize, Serialize)]
pub(crate) struct CachedRequest {
    pub(crate) request_hash: [u8; 32],
    pub(crate) response: Option<ResponseEnvelope>,
}

impl ResponseCache {
    pub(crate) fn new(cache: Arc<dyn ICache>) -> Self {
        Self { cache }
    }

    pub(crate) async fn begin(&self, scope: &str, request_span_id: &str, request_hash: [u8; 32]) -> CacheDisposition {
        let key = ResponseCache::response_cache_key(scope, request_span_id);
        let pending = CachedRequest {
            request_hash,
            response: None,
        };
        let Ok(encoded) = serde_json::to_vec(&pending) else {
            tracing::warn!("failed to encode MQTT response-cache entry; continuing without cache");
            return CacheDisposition::New;
        };
        match self
            .cache
            .set_if_absent("mqtt_response", &key, encoded, RESPONSE_CACHE_TTL)
            .await
        {
            Ok(true) => return CacheDisposition::New,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, "MQTT response cache is unavailable; relying on SQL idempotency");
                return CacheDisposition::New;
            }
        }
        let existing = match self.cache.get("mqtt_response", &key).await {
            Ok(Some(existing)) => existing,
            Ok(None) => return CacheDisposition::New,
            Err(error) => {
                tracing::warn!(%error, "MQTT response cache is unavailable; relying on SQL idempotency");
                return CacheDisposition::New;
            }
        };
        let Ok(existing) = serde_json::from_slice::<CachedRequest>(&existing) else {
            tracing::warn!("discarding malformed non-authoritative MQTT response-cache entry");
            let _ = self.cache.delete("mqtt_response", &key).await;
            return CacheDisposition::New;
        };
        if existing.request_hash != request_hash {
            CacheDisposition::Conflict
        } else if let Some(response) = existing.response {
            CacheDisposition::Cached(response)
        } else {
            CacheDisposition::InFlight
        }
    }

    pub(crate) async fn complete(&self, scope: &str, request_span_id: &str, response: ResponseEnvelope) {
        let key = ResponseCache::response_cache_key(scope, request_span_id);
        let existing = match self.cache.get("mqtt_response", &key).await {
            Ok(Some(existing)) => existing,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "MQTT response cache is unavailable; response remains durable through SQL");
                return;
            }
        };
        let Ok(mut entry) = serde_json::from_slice::<CachedRequest>(&existing) else {
            let _ = self.cache.delete("mqtt_response", &key).await;
            return;
        };
        entry.response = Some(response);
        let Ok(encoded) = serde_json::to_vec(&entry) else {
            return;
        };
        if let Err(error) = self.cache.set("mqtt_response", &key, encoded, RESPONSE_CACHE_TTL).await {
            tracing::warn!(%error, "MQTT response cache is unavailable; response remains durable through SQL");
        }
    }
}

impl ResponseCache {
    pub(crate) fn response_cache_key(scope: &str, request_span_id: &str) -> String {
        let mut digest = Sha256::new();
        digest.update(scope.as_bytes());
        digest.update([0]);
        digest.update(request_span_id.as_bytes());
        hex::encode(digest.finalize())
    }

    pub(crate) fn digest(value: &[u8]) -> [u8; 32] {
        Sha256::digest(value).into()
    }
}

#[cfg(test)]
#[path = "../../../tests/transport/mqtt/state_test.rs"]
mod tests;
