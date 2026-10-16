use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use crate::{
    AssetIdentity, HttpMethod, PayerConfig, TransportError, TransportRequest, TransportResponse, VaultTransport,
};

use super::*;
use crate::IdempotencyKey;

#[derive(Clone)]
struct CaptureTransport(Arc<Mutex<Option<TransportRequest>>>);

impl VaultTransport for CaptureTransport {
    async fn send(&self, request: TransportRequest) -> std::result::Result<TransportResponse, TransportError> {
        *self.0.lock().expect("capture") = Some(request);
        Ok(TransportResponse {
            status: 409,
            headers: BTreeMap::new(),
            body: br#"{"error":{"code":"IDEMPOTENCY_CONFLICT","message":"different request hash"}}"#.to_vec(),
        })
    }
}

#[tokio::test]
async fn payer_repeats_stable_key_in_header_and_body() {
    let captured = Arc::new(Mutex::new(None));
    let config = PayerConfig::new(
        "scenario-1",
        "wallet-1",
        "x402",
        AssetIdentity {
            network: "eip155:31337".to_owned(),
            asset: "USDC".to_owned(),
            token_contract: None,
            decimals: 6,
        },
        "1000",
        "0x0000000000000000000000000000000000000001",
    )
    .expect("config");
    let middleware = PayerMiddleware::new(CaptureTransport(Arc::clone(&captured)), config).expect("middleware");
    let key = IdempotencyKey::for_operation("run-1", "paid").expect("key");
    let request = AuthorizePaymentRequest {
        context: OperationContext::new("scenario-1", key.clone())
            .expect("context")
            .with_run_id("run-1"),
        wallet_id: "wallet-1".to_owned(),
        protocol: "x402".to_owned(),
        asset: AssetIdentity {
            network: "eip155:31337".to_owned(),
            asset: "USDC".to_owned(),
            token_contract: None,
            decimals: 6,
        },
        amount: "1000".to_owned(),
        recipient: "0x0000000000000000000000000000000000000001".to_owned(),
        resource: "https://merchant.example/mcp".to_owned(),
        challenge: PaymentRequired {
            protocol_hint: Some("x402".to_owned()),
            origin: "https://merchant.example".to_owned(),
            resource: "https://merchant.example/mcp".to_owned(),
            content_type: "application/json".to_owned(),
            headers: BTreeMap::new(),
            body: "{}".to_owned(),
            expires_at: None,
        },
    };
    assert!(middleware
        .authorize_payment(&request)
        .await
        .expect_err("conflict")
        .is_conflict());
    let sent = captured.lock().expect("capture").clone().expect("request");
    assert_eq!(sent.method, HttpMethod::Post);
    assert_eq!(
        sent.headers.get("idempotency-key").map(String::as_str),
        Some(key.as_str())
    );
    let body: serde_json::Value = serde_json::from_slice(&sent.body).expect("JSON");
    assert_eq!(body["idempotency_key"], key.as_str());
    assert!(body.get("client_id").is_none());
}

#[tokio::test]
async fn payer_rejects_protocol_hint_mismatch_before_vault_call() {
    let captured = Arc::new(Mutex::new(None));
    let config = PayerConfig::new(
        "scenario-1",
        "wallet-1",
        "x402",
        AssetIdentity {
            network: "eip155:31337".to_owned(),
            asset: "USDC".to_owned(),
            token_contract: None,
            decimals: 6,
        },
        "1000",
        "0x0000000000000000000000000000000000000001",
    )
    .expect("config");
    let middleware = PayerMiddleware::new(CaptureTransport(Arc::clone(&captured)), config).expect("middleware");
    let error = middleware
        .authorize(
            &PayerHttpRequest {
                method: "POST".to_owned(),
                url: "https://merchant.example/mcp".to_owned(),
                origin: "https://merchant.example".to_owned(),
                headers: BTreeMap::new(),
                body: Vec::new(),
                follow_redirects: false,
            },
            &PayerHttpResponse {
                status: 402,
                headers: BTreeMap::from([("payment-protocol".to_owned(), "mpp".to_owned())]),
                body: b"{}".to_vec(),
            },
            &PayerRequestContext::for_operation("run-1", "paid").expect("context"),
        )
        .await
        .expect_err("mismatched protocol");
    assert!(matches!(error, Error::UnsafeRetry(_)));
    assert!(captured.lock().expect("capture").is_none());
}
