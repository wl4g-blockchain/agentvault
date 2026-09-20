use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};

use crate::{
    config::SecurityConfig,
    protocol::{SignRequest, SignResponse},
    EncryptedFileStore, Error,
};

#[derive(Clone)]
struct CachedResponse {
    fingerprint: [u8; 32],
    response: SignResponse,
    retain_until: i64,
}

pub struct WalletService {
    store: Arc<EncryptedFileStore>,
    security: SecurityConfig,
    responses: Mutex<HashMap<uuid::Uuid, CachedResponse>>,
}

impl WalletService {
    pub fn new(store: Arc<EncryptedFileStore>, security: SecurityConfig) -> Self {
        Self {
            store,
            security,
            responses: Mutex::new(HashMap::new()),
        }
    }

    pub fn max_request_bytes(&self) -> usize {
        self.security.max_request_bytes
    }

    pub fn handle_json(&self, payload: &[u8]) -> Option<SignResponse> {
        if payload.len() > self.security.max_request_bytes {
            tracing::warn!(payload_bytes = payload.len(), "discarding oversized signing request");
            return None;
        }
        let request: SignRequest = match serde_json::from_slice(payload) {
            Ok(request) => request,
            Err(error) => {
                tracing::warn!(%error, "discarding malformed signing request");
                return None;
            }
        };
        Some(self.handle(&request))
    }

    #[must_use]
    pub fn handle(&self, request: &SignRequest) -> SignResponse {
        let now = unix_timestamp();
        let fingerprint = request.fingerprint();
        let mut responses = self.responses.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        responses.retain(|_, cached| cached.retain_until >= now);
        if let Some(cached) = responses.get(&request.request_id) {
            if cached.fingerprint == fingerprint {
                return cached.response.clone();
            }
            return SignResponse::failure(request, &Error::RequestConflict);
        }

        let response = match request.validate(
            now,
            self.security.max_request_ttl_seconds,
            self.security.clock_skew_seconds,
            &self.security.client_policies,
        ) {
            Ok(digest) => match self.store.sign_digest(&request.wallet_id, &digest) {
                Ok((key, signature)) => {
                    SignResponse::success(request, key.address, format!("0x{}", hex::encode(signature)), now)
                }
                Err(error) => SignResponse::failure(request, &error),
            },
            Err(error) => SignResponse::failure(request, &error),
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

    use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
    use sha2::{Digest, Sha256};
    use tempfile::TempDir;
    use uuid::Uuid;

    use crate::{
        protocol::{ClientPolicy, SignRequest, EIP712_SECP256K1, PROTOCOL_VERSION},
        store::generate_master_key_file,
    };

    use super::*;

    fn service() -> (TempDir, WalletService) {
        let temp = TempDir::new().unwrap();
        let master = temp.path().join("master.key");
        generate_master_key_file(&master).unwrap();
        let store = Arc::new(EncryptedFileStore::open(temp.path().join("store"), master).unwrap());
        store.generate("payer").unwrap();
        let security = SecurityConfig {
            client_policies: vec![ClientPolicy {
                client_id_prefix: "test-".to_owned(),
                wallet_ids: vec!["payer".to_owned()],
                purposes: vec!["x402.payment".to_owned()],
            }],
            ..SecurityConfig::default()
        };
        (temp, WalletService::new(store, security))
    }

    fn request(now: i64) -> SignRequest {
        SignRequest {
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
    fn signs_and_replays_identical_request() {
        let (_temp, service) = service();
        let request = request(unix_timestamp());
        let first = service.handle(&request);
        let replay = service.handle(&request);
        assert!(first.error.is_none());
        assert_eq!(first.signature, replay.signature);
    }

    #[test]
    fn rejects_request_id_reuse() {
        let (_temp, service) = service();
        let first = request(unix_timestamp());
        let mut conflicting = first.clone();
        conflicting.digest_b64 = BASE64.encode([9_u8; 32]);
        let _ = service.handle(&first);
        let response = service.handle(&conflicting);
        assert_eq!(response.error.unwrap().code, "REQUEST_ID_CONFLICT");
    }
}
