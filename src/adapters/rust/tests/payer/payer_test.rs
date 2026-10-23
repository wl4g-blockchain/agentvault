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

#[derive(Clone)]
struct ReportCaptureTransport(Arc<Mutex<Option<TransportRequest>>>);

impl VaultTransport for ReportCaptureTransport {
    async fn send(&self, request: TransportRequest) -> std::result::Result<TransportResponse, TransportError> {
        *self.0.lock().expect("capture") = Some(request);
        let intent = serde_json::json!({
            "id": "intent", "scenario_id": "scenario-1", "run_id": "run-1",
            "wallet_id": "wallet", "policy_id": "policy", "direction": "outbound",
            "protocol": "mpp",
            "asset": {"network": "eip155:42431", "asset": "USDC", "decimals": 6},
            "recipient": "0x1111111111111111111111111111111111111111",
            "amount": "1000", "settled_amount": "0",
            "resource": "https://merchant.example/tool", "challenge_hash": "challenge",
            "request_hash": "request", "status": "pending_settlement",
            "reservation_status": "reserved", "reserved_amount": "1000",
            "created_at": 1, "updated_at": 1
        });
        Ok(TransportResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: serde_json::to_vec(&serde_json::json!({
                "intent": intent, "accepted": true, "replayed": false
            }))
            .expect("response JSON"),
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
    let keys = body
        .as_object()
        .expect("object")
        .keys()
        .map(String::as_str)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        keys,
        std::collections::BTreeSet::from(["challenge", "run_id", "scenario_id"])
    );
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

#[tokio::test]
async fn payer_forwards_native_challenge_headers_to_agentvault() {
    for (header, value) in [
        ("payment-required", "eyJ4NDAyVmVyc2lvbiI6Mn0="),
        ("www-authenticate", r#"Payment id="charge", method="tempo""#),
    ] {
        let captured = Arc::new(Mutex::new(None));
        let middleware = PayerMiddleware::new(
            CaptureTransport(Arc::clone(&captured)),
            PayerConfig::new("scenario-1").expect("config"),
        )
        .expect("middleware");
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
                    headers: BTreeMap::from([(header.to_owned(), value.to_owned())]),
                    body: b"{}".to_vec(),
                },
                &PayerRequestContext::new("run-1").expect("context"),
            )
            .await
            .expect_err("transport returns conflict");
        assert!(error.is_conflict());
        let sent = captured.lock().expect("capture").clone().expect("request");
        let body: serde_json::Value = serde_json::from_slice(&sent.body).expect("JSON");
        assert_eq!(body["challenge"]["headers"][header], value);
    }
}

#[tokio::test]
async fn payer_rejects_mcp_challenges_with_a_different_jsonrpc_identity() {
    let captured = Arc::new(Mutex::new(None));
    let middleware = PayerMiddleware::new(
        CaptureTransport(Arc::clone(&captured)),
        PayerConfig::new("scenario-1").expect("config"),
    )
    .expect("middleware");
    let original = PayerHttpRequest {
        method: "POST".to_owned(),
        url: "https://merchant.example/mcp".to_owned(),
        origin: "https://merchant.example".to_owned(),
        headers: BTreeMap::new(),
        body: br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"marketpulse.realtime"}}"#.to_vec(),
        follow_redirects: false,
    };
    for body in [
        br#"{"jsonrpc":"2.0","id":2,"result":{"isError":true,"structuredContent":{"x402Version":2,"resource":{"url":"mcp://tool/marketpulse.realtime"},"accepts":[]}}}"#.as_slice(),
        br#"{"jsonrpc":"1.0","id":1,"result":{"isError":true,"structuredContent":{"x402Version":2,"resource":{"url":"mcp://tool/marketpulse.realtime"},"accepts":[]}}}"#.as_slice(),
    ] {
        let payment = middleware
            .authorize_mcp(
                &original,
                &PayerHttpResponse {
                    status: 200,
                    headers: BTreeMap::new(),
                    body: body.to_vec(),
                },
                &PayerRequestContext::new("run-1").expect("context"),
            )
            .await
            .expect("mismatched response is ignored");
        assert!(payment.is_none());
    }
    assert!(captured.lock().expect("capture").is_none());
}

#[tokio::test]
async fn payer_reports_mpp_receipts_without_x402_projection() {
    let captured = Arc::new(Mutex::new(None));
    let middleware = PayerMiddleware::new(
        ReportCaptureTransport(Arc::clone(&captured)),
        PayerConfig::new("scenario-1").expect("config"),
    )
    .expect("middleware");
    let intent = serde_json::from_value(serde_json::json!({
        "id": "intent", "scenario_id": "scenario-1", "run_id": "run-1",
        "wallet_id": "wallet", "policy_id": "policy", "direction": "outbound",
        "protocol": "mpp",
        "asset": {"network": "eip155:42431", "asset": "USDC", "decimals": 6},
        "recipient": "0x1111111111111111111111111111111111111111",
        "amount": "1000", "settled_amount": "0",
        "resource": "https://merchant.example/tool", "challenge_hash": "challenge",
        "request_hash": "request", "status": "signed", "reservation_status": "reserved",
        "reserved_amount": "1000", "created_at": 1, "updated_at": 1
    }))
    .expect("intent");
    let challenge = mpp::PaymentChallenge::new(
        "challenge",
        "merchant.example",
        "tempo",
        "charge",
        mpp::Base64UrlJson::from_value(&serde_json::json!({"amount": "1000", "currency": "USDC"}))
            .expect("MPP request"),
    )
    .with_header(mpp::PAYMENT_AUTHORIZATION_HEADER);
    let authorization = mpp::format_authorization(&mpp::PaymentCredential::new(
        challenge.to_echo(),
        mpp::PaymentPayload::hash("0x1111"),
    ))
    .expect("MPP authorization");
    let payment = AuthorizedPayment {
        intent,
        credential: PaymentCredential {
            protocol: "mpp".to_owned(),
            challenge_hash: "challenge".to_owned(),
            credential_hash: "credential".to_owned(),
            payload: serde_json::json!({"authorization": authorization}),
            http: Some(crate::HttpCredential {
                bound_origin: "https://merchant.example".to_owned(),
                header_name: "payment-authorization".to_owned(),
                header_value: authorization.clone(),
            }),
        },
    };
    let existing = BTreeMap::from([("authorization".to_owned(), "Bearer app-token".to_owned())]);
    let (name, _) = middleware
        .credential_header(&payment, "https://merchant.example", &existing)
        .expect("dedicated MPP credential");
    assert_eq!(name, "payment-authorization");

    let receipt = mpp::format_receipt(&mpp::Receipt::success("tempo", "0x1111")).expect("MPP receipt");
    middleware
        .report(
            &PayerRequestContext::new("run-1").expect("context"),
            &payment,
            &PayerHttpResponse {
                status: 200,
                headers: BTreeMap::from([("payment-receipt".to_owned(), receipt)]),
                body: Vec::new(),
            },
        )
        .await
        .expect("report MPP receipt");
    let request = captured.lock().expect("capture").clone().expect("request");
    let body: serde_json::Value = serde_json::from_slice(&request.body).expect("request JSON");
    assert_eq!(body["protocol"], "mpp");
    assert_eq!(body["evidence"]["header_name"], "payment-receipt");
    assert!(body.get("payment_response").is_none());
}
