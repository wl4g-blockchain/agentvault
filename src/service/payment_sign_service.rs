use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Instant, SystemTime, UNIX_EPOCH},
};

use crate::{
    config::SecurityConfig,
    history::SignHistoryStore,
    model::sign::{
        PaymentSignHistoryPage, PaymentSignRecord, PaymentSignRequest, PaymentSignResponse, SignHistoryQuery,
        SignTransport,
    },
    observability::AgentVaultMetrics,
    EncryptedFileStore, Error,
};

#[derive(Clone)]
struct CachedResponse {
    fingerprint: [u8; 32],
    response: PaymentSignResponse,
    retain_until: i64,
}

/// Protocol validation, deduplication, key signing, audit history, and metrics.
pub struct PaymentSignService {
    store: Arc<EncryptedFileStore>,
    history: Arc<SignHistoryStore>,
    metrics: AgentVaultMetrics,
    security: SecurityConfig,
    responses: Mutex<HashMap<uuid::Uuid, CachedResponse>>,
}

impl PaymentSignService {
    #[must_use]
    pub fn new(
        store: Arc<EncryptedFileStore>,
        history: Arc<SignHistoryStore>,
        metrics: AgentVaultMetrics,
        security: SecurityConfig,
    ) -> Self {
        metrics.set_history_records(history.len());
        Self {
            store,
            history,
            metrics,
            security,
            responses: Mutex::new(HashMap::new()),
        }
    }

    #[must_use]
    pub const fn max_request_bytes(&self) -> usize {
        self.security.max_request_bytes
    }

    pub fn handle_json(&self, payload: &[u8], transport: SignTransport) -> Option<PaymentSignResponse> {
        if payload.len() > self.security.max_request_bytes {
            tracing::warn!(payload_bytes = payload.len(), "discarding oversized signing request");
            return None;
        }
        let request: PaymentSignRequest = match serde_json::from_slice(payload) {
            Ok(request) => request,
            Err(error) => {
                tracing::warn!(%error, "discarding malformed signing request");
                return None;
            }
        };
        Some(self.execute(&request, transport))
    }

    #[must_use]
    pub fn execute(&self, request: &PaymentSignRequest, transport: SignTransport) -> PaymentSignResponse {
        let span = tracing::info_span!(
            "payment.sign",
            request.id = %request.request_id,
            wallet.id = %request.wallet_id,
            payment.purpose = %request.purpose,
            signer.transport = transport.as_str(),
            sign.outcome = tracing::field::Empty,
        );
        let _entered = span.enter();
        let started = Instant::now();
        self.metrics.sign_started();
        let now = unix_timestamp();
        let response = self.execute_inner(request, now);
        span.record("sign.outcome", response.outcome().as_str());
        let record = PaymentSignRecord::from_exchange(request, &response, transport, now);
        if let Err(error) = self.history.record(record) {
            self.metrics.history_persistence_failed();
            tracing::error!(%error, request.id = %request.request_id, "failed to persist signing audit record");
        }
        self.metrics.set_history_records(self.history.len());
        self.metrics
            .sign_finished(transport, response.outcome(), started.elapsed().as_secs_f64());
        response
    }

    /// Searches durable signing audit metadata.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid filters or pagination.
    pub fn search_history(&self, query: &SignHistoryQuery) -> crate::Result<PaymentSignHistoryPage> {
        self.history.search(query)
    }

    #[must_use]
    pub fn get_history(&self, request_id: uuid::Uuid) -> Option<PaymentSignRecord> {
        self.history.get_by_request_id(request_id)
    }

    /// Checks the key store and audit file for management readiness.
    ///
    /// # Errors
    ///
    /// Returns an error when either durable dependency cannot be read safely.
    pub fn readiness(&self) -> crate::Result<()> {
        self.store.list()?;
        self.history.readiness()
    }

    fn execute_inner(&self, request: &PaymentSignRequest, now: i64) -> PaymentSignResponse {
        let fingerprint = request.fingerprint();
        let mut responses = self.responses.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        responses.retain(|_, cached| cached.retain_until >= now);
        if let Some(cached) = responses.get(&request.request_id) {
            if cached.fingerprint == fingerprint {
                return cached.response.clone();
            }
            return PaymentSignResponse::failure(request, &Error::RequestConflict);
        }

        let response = match request.validate(
            now,
            self.security.max_request_ttl_seconds,
            self.security.clock_skew_seconds,
            &self.security.client_policies,
        ) {
            Ok(digest) => match self.store.sign_digest(&request.wallet_id, &digest) {
                Ok((key, signature)) => {
                    PaymentSignResponse::success(request, key.address, format!("0x{}", hex::encode(signature)), now)
                }
                Err(error) => PaymentSignResponse::failure(request, &error),
            },
            Err(error) => PaymentSignResponse::failure(request, &error),
        };

        if responses.len() >= self.security.max_dedup_entries {
            if let Some(oldest) = responses
                .iter()
                .min_by_key(|(_, cached)| cached.retain_until)
                .map(|(request_id, _)| *request_id)
            {
                responses.remove(&oldest);
            }
        }
        responses.insert(
            request.request_id,
            CachedResponse {
                fingerprint,
                response: response.clone(),
                retain_until: now.saturating_add(self.security.dedup_ttl_seconds),
            },
        );
        response
    }
}

fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use sha2::{Digest as _, Sha256};
    use tempfile::TempDir;
    use uuid::Uuid;

    use crate::{
        model::sign::{ClientPolicy, EIP712_SECP256K1, PROTOCOL_VERSION},
        store::generate_master_key_file,
    };

    use super::*;

    fn service() -> (TempDir, PaymentSignService) {
        let temp = TempDir::new().unwrap();
        let master = temp.path().join("master.key");
        generate_master_key_file(&master).unwrap();
        let store = Arc::new(EncryptedFileStore::open(temp.path().join("store"), master).unwrap());
        store.generate("payer").unwrap();
        let history = Arc::new(SignHistoryStore::open(temp.path().join("store/history.jsonl"), 100).unwrap());
        let security = SecurityConfig {
            client_policies: vec![ClientPolicy {
                client_id_prefix: "test-".to_owned(),
                wallet_ids: vec!["payer".to_owned()],
                purposes: vec!["x402.payment".to_owned()],
            }],
            ..SecurityConfig::default()
        };
        (
            temp,
            PaymentSignService::new(store, history, AgentVaultMetrics::default(), security),
        )
    }

    fn request(now: i64) -> PaymentSignRequest {
        PaymentSignRequest {
            version: PROTOCOL_VERSION.to_owned(),
            request_id: Uuid::new_v4(),
            client_id: "test-client".to_owned(),
            wallet_id: "payer".to_owned(),
            purpose: "x402.payment".to_owned(),
            scheme: EIP712_SECP256K1.to_owned(),
            digest_b64: BASE64.encode(Sha256::digest(b"payment")),
            issued_at: now,
            expires_at: now + 30,
            metadata: BTreeMap::new(),
        }
    }

    #[test]
    fn signs_replays_and_records_attempts() {
        let (_temp, service) = service();
        let request = request(unix_timestamp());
        let first = service.execute(&request, SignTransport::Local);
        let replay = service.execute(&request, SignTransport::Local);
        assert!(first.error.is_none());
        assert_eq!(first.signature, replay.signature);
        assert_eq!(service.search_history(&SignHistoryQuery::default()).unwrap().total, 2);
    }

    #[test]
    fn rejects_request_id_reuse() {
        let (_temp, service) = service();
        let first = request(unix_timestamp());
        let mut conflicting = first.clone();
        conflicting.digest_b64 = BASE64.encode([9_u8; 32]);
        let _ = service.execute(&first, SignTransport::Local);
        let response = service.execute(&conflicting, SignTransport::Local);
        assert_eq!(response.error.unwrap().code, "REQUEST_ID_CONFLICT");
    }
}
