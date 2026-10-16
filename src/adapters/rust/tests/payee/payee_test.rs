use std::sync::{Arc, Mutex};

use crate::{
    Error, IdempotencyKey, PayeeConfig, PayeeMiddleware, PayeeOperation, TransportError, TransportRequest,
    TransportResponse, VaultTransport,
};

#[derive(Clone, Default)]
struct RejectingTransport(Arc<Mutex<Vec<String>>>);

impl VaultTransport for RejectingTransport {
    async fn send(&self, request: TransportRequest) -> std::result::Result<TransportResponse, TransportError> {
        self.0.lock().expect("paths").push(request.path.clone());
        let body = match request.path.as_str() {
            "/api/v1/payee/challenges" => {
                br#"{"scenario_id":"scenario","offer_id":"offer","resource":"mcp://marketpulse/realtime","protocol":"x402","asset":{"network":"eip155:31337","asset":"USDC","decimals":6},"amount":"1000","recipient":"0x1","nonce":"nonce","challenge_hash":"challenge","expires_at":1,"payload":{"payment_required":{}}}"#.to_vec()
            }
            "/api/v1/payee/verify" => {
                br#"{"intent":{"id":"intent","scenario_id":"scenario","run_id":"run","offer_id":"offer","direction":"inbound","protocol":"x402","asset":{"network":"eip155:31337","asset":"USDC","decimals":6},"recipient":"0x1","amount":"1000","settled_amount":"0","resource":"mcp://marketpulse/realtime","challenge_hash":"challenge","idempotency_key":"execution.challenge","request_hash":"request-hash","status":"pending_settlement","reservation_status":"reserved","reserved_amount":"1000","created_at":1,"updated_at":1},"valid":false}"#.to_vec()
            }
            other => return Err(TransportError::new(format!("unexpected Vault call: {other}"))),
        };
        Ok(TransportResponse {
            status: 200,
            headers: std::collections::BTreeMap::new(),
            body,
        })
    }
}

#[test]
fn payee_derives_distinct_stable_phase_keys() {
    let operation = PayeeOperation {
        scenario_id: "scenario".to_owned(),
        run_id: "run".to_owned(),
        execution_key: "execution".to_owned(),
        offer_id: "offer".to_owned(),
        resource: "mcp://marketpulse/realtime".to_owned(),
        origin: "https://marketpulse.example".to_owned(),
        protocol: "x402".to_owned(),
        request_hash: "hash".to_owned(),
        credential_payload: None,
    };
    assert_eq!(
        operation.challenge_key().expect("key"),
        IdempotencyKey::new("execution.challenge").expect("key")
    );
    assert_eq!(
        operation.settlement_key().expect("key"),
        IdempotencyKey::new("execution.settle").expect("key")
    );
}

#[tokio::test]
async fn payee_rejects_invalid_payment_before_settlement() {
    let transport = RejectingTransport::default();
    let paths = Arc::clone(&transport.0);
    let middleware = PayeeMiddleware::new(transport, PayeeConfig::default()).expect("middleware");
    let error = middleware
        .protect(&PayeeOperation {
            scenario_id: "scenario".to_owned(),
            run_id: "run".to_owned(),
            execution_key: "execution".to_owned(),
            offer_id: "offer".to_owned(),
            resource: "mcp://marketpulse/realtime".to_owned(),
            origin: "https://marketpulse.example".to_owned(),
            protocol: "x402".to_owned(),
            request_hash: "request-hash".to_owned(),
            credential_payload: Some(serde_json::json!({"signature": "present"})),
        })
        .await
        .expect_err("invalid payment");
    assert!(matches!(error, Error::Api { status: 402, .. }));
    assert_eq!(
        *paths.lock().expect("paths"),
        vec!["/api/v1/payee/challenges".to_owned(), "/api/v1/payee/verify".to_owned()]
    );
}
