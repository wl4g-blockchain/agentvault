use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use crate::{HttpMethod, PayerConfig, TransportError, TransportRequest, TransportResponse, VaultTransport};

use super::*;

#[derive(Clone)]
struct CaptureTransport(Arc<Mutex<Option<TransportRequest>>>);

impl VaultTransport for CaptureTransport {
    async fn send(&self, request: TransportRequest) -> std::result::Result<TransportResponse, TransportError> {
        *self.0.lock().expect("capture") = Some(request);
        Ok(TransportResponse {
            status: 409,
            headers: BTreeMap::new(),
            body: br#"{"error":{"code":"PAYMENT_CONFLICT","message":"different request hash"}}"#.to_vec(),
        })
    }
}

#[tokio::test]
async fn payer_uses_run_id_as_the_only_replay_identity() {
    let captured = Arc::new(Mutex::new(None));
    let config = PayerConfig::new("scenario-1").expect("config");
    let middleware = PayerMiddleware::new(CaptureTransport(Arc::clone(&captured)), config).expect("middleware");
    let request = AuthorizePaymentRequest {
        context: OperationContext::new("scenario-1", "run-1").expect("context"),
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
    let body: serde_json::Value = serde_json::from_slice(&sent.body).expect("JSON");
    assert_eq!(body["run_id"], "run-1");
    let keys = body.as_object().expect("object").keys().cloned().collect::<Vec<_>>();
    assert_eq!(keys, ["challenge", "run_id", "scenario_id"]);
}

#[tokio::test]
async fn payer_forwards_captured_protocol_hint_without_local_projection() {
    let captured = Arc::new(Mutex::new(None));
    let config = PayerConfig::new("scenario-1").expect("config");
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
            &PayerRequestContext::new("run-1").expect("context"),
        )
        .await
        .expect_err("transport returns conflict");
    assert!(error.is_conflict());
    let sent = captured.lock().expect("capture").clone().expect("request");
    let body: serde_json::Value = serde_json::from_slice(&sent.body).expect("JSON");
    assert_eq!(body["challenge"]["protocol_hint"], "mpp");
}
