use axum::{
    body::{to_bytes, Body},
    http::{header, Response, StatusCode},
};

use crate::{
    PayeeConfig, PayeeMiddleware, PaymentChallenge, SettledIncomingPayment, TransportError, TransportRequest,
    TransportResponse, VaultTransport,
};

use super::*;

#[derive(Clone)]
struct UnusedTransport;

impl VaultTransport for UnusedTransport {
    async fn send(&self, _request: TransportRequest) -> Result<TransportResponse, TransportError> {
        Err(TransportError::new("unexpected Vault call"))
    }
}

#[test]
fn mcp_challenge_carries_stable_payment_profile_metadata() {
    let core = PayeeMiddleware::new(UnusedTransport, PayeeConfig::default()).expect("payee middleware");
    let challenge: PaymentChallenge = serde_json::from_value(serde_json::json!({
        "scenario_id": "scenario", "price_id": "price", "resource": "mcp://tool/marketpulse.realtime",
        "protocol": "x402", "asset": {"network": "eip155:42431", "asset": "USDC", "decimals": 6},
        "amount": "1000", "recipient": "0x1111111111111111111111111111111111111111",
        "nonce": "nonce", "challenge_hash": "challenge", "expires_at": 2_000_000_000,
        "payload": {"payment_required": {
            "x402Version": 2,
            "resource": {"url": "mcp://tool/marketpulse.realtime", "mimeType": "application/json"},
            "accepts": [{
                "scheme": "exact", "network": "eip155:42431", "amount": "1000",
                "payTo": "0x1111111111111111111111111111111111111111", "maxTimeoutSeconds": 60,
                "asset": "0x2222222222222222222222222222222222222222"
            }]
        }}
    }))
    .expect("challenge");
    let response = AxumProtocolResponse::mcp_challenge(&core, &challenge, &serde_json::json!(1));
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["payment-protocol"], "x402");
    assert_eq!(response.headers()["payment-expires-at"], "2000000000");
    assert!(response.headers().get("payment-required").is_none());
}

#[test]
fn mcp_replay_body_removes_only_protocol_owned_payment_metadata() {
    let call = McpCall::parse(
        br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"arguments":{"z":2,"a":1},"name":"marketpulse.realtime","_meta":{"application/trace":"preserved","x402/payment":{"x402Version":2},"org.paymentauth/credential":{"payload":{}}}}}"#,
        "marketpulse.realtime",
        &["x402/payment", "org.paymentauth/credential"],
        &["org.paymentauth/credential"],
    )
    .expect("valid MCP call")
    .expect("matching paid tool");
    let replayed: serde_json::Value = serde_json::from_slice(&call.canonical_body).expect("canonical MCP body");
    assert_eq!(
        replayed.pointer("/params/_meta/application~1trace"),
        Some(&serde_json::json!("preserved"))
    );
    assert!(replayed.pointer("/params/_meta/x402~1payment").is_none());
    assert!(replayed.pointer("/params/_meta/org.paymentauth~1credential").is_none());

    let reordered = McpCall::parse(
        br#"{"params":{"_meta":{"application/trace":"preserved"},"name":"marketpulse.realtime","arguments":{"a":1,"z":2}},"method":"tools/call","id":1,"jsonrpc":"2.0"}"#,
        "marketpulse.realtime",
        &["x402/payment", "org.paymentauth/credential"],
        &["org.paymentauth/credential"],
    )
    .expect("valid reordered MCP call")
    .expect("matching reordered tool");
    assert_eq!(call.canonical_body, reordered.canonical_body);
}

#[test]
fn mcp_call_accepts_mpp_root_metadata_and_rejects_ambiguous_placement() {
    let call = McpCall::parse(
        br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","_meta":{"application/root":"preserved","org.paymentauth/credential":{"payload":{"challenge":{"id":"challenge"}}}},"params":{"name":"marketpulse.realtime","arguments":{},"_meta":{"application/trace":"preserved"}}}"#,
        "marketpulse.realtime",
        &["x402/payment", "org.paymentauth/credential"],
        &["org.paymentauth/credential"],
    )
    .expect("valid root MPP credential")
    .expect("matching paid tool");
    assert!(call.meta.pointer("/org.paymentauth~1credential").is_some());
    let replayed: serde_json::Value = serde_json::from_slice(&call.canonical_body).expect("canonical MCP body");
    assert_eq!(
        replayed.pointer("/_meta/application~1root"),
        Some(&serde_json::json!("preserved"))
    );
    assert!(replayed.pointer("/_meta/org.paymentauth~1credential").is_none());
    assert_eq!(
        replayed.pointer("/params/_meta/application~1trace"),
        Some(&serde_json::json!("preserved"))
    );

    let ambiguous = McpCall::parse(
        br#"{"jsonrpc":"2.0","id":1,"method":"tools/call","_meta":{"org.paymentauth/credential":{"payload":{}}},"params":{"name":"marketpulse.realtime","_meta":{"org.paymentauth/credential":{"payload":{}}}}}"#,
        "marketpulse.realtime",
        &["x402/payment", "org.paymentauth/credential"],
        &["org.paymentauth/credential"],
    )
    .err()
    .expect("duplicate root and nested credentials must fail closed");
    assert_eq!(ambiguous.code, "PAYMENT_INVALID");
}

#[tokio::test]
async fn paid_mcp_business_failure_remains_retryable_after_settlement() {
    let core = PayeeMiddleware::new(UnusedTransport, PayeeConfig::default()).expect("payee middleware");
    let challenge: PaymentChallenge = serde_json::from_value(serde_json::json!({
        "scenario_id": "scenario", "price_id": "price", "resource": "mcp://tool/realtime",
        "protocol": "x402", "asset": {"network": "eip155:31337", "asset": "USDC", "decimals": 6},
        "amount": "1000", "recipient": "0x1", "nonce": "nonce", "challenge_hash": "challenge",
        "expires_at": 2_000_000_000, "payload": {}
    }))
    .expect("challenge");
    let payment: SettledIncomingPayment = serde_json::from_value(serde_json::json!({
        "intent": {
            "id": "intent", "scenario_id": "scenario", "run_id": "run", "price_id": "price",
            "direction": "inbound", "protocol": "x402",
            "asset": {"network": "eip155:31337", "asset": "USDC", "decimals": 6},
            "recipient": "0x1", "amount": "1000", "settled_amount": "1000",
            "resource": "mcp://tool/realtime", "challenge_hash": "challenge", "request_hash": "request",
            "status": "settled", "reservation_status": "consumed", "reserved_amount": "0",
            "tx_hash": "0x1", "created_at": 1, "updated_at": 2, "settled_at": 2
        },
        "receipt": {
            "intent_id": "intent", "scenario_id": "scenario", "direction": "inbound", "protocol": "x402",
            "asset": {"network": "eip155:31337", "asset": "USDC", "decimals": 6},
            "recipient": "0x1", "payer": "0x2", "requested_amount": "1000", "settled_amount": "1000",
            "status": "settled", "tx_hash": "0x1", "evidence_hash": "evidence", "confirmed_at": 2
        }
    }))
    .expect("payment");
    let body = br#"{"jsonrpc":"2.0","id":1,"error":{"code":-32004,"message":"retry delivery"}}"#;
    let response = Response::builder()
        .status(StatusCode::SERVICE_UNAVAILABLE)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.as_slice()))
        .expect("response");
    let response =
        AxumProtocolResponse::mcp_receipt(&core, &challenge, &payment, &serde_json::json!(1), response, 4096).await;
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let actual = to_bytes(response.into_body(), 4096).await.expect("body");
    assert_eq!(actual.as_ref(), body);
}
