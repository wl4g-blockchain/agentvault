use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};

use axum::{
    body::Body,
    extract::Json,
    http::{HeaderMap as AxumHeaderMap, Response},
    response::Redirect,
    routing::post,
    Router,
};
use serde_json::json;

use super::*;

const CONTRACT: &str = "0x0000000000000000000000000000000000001001";
const PAY_TO: &str = "0x0000000000000000000000000000000000002002";
const PAYER: &str = "0x0000000000000000000000000000000000003003";

#[tokio::test]
async fn sends_the_official_v2_wire_and_maps_trusted_verify_evidence() {
    let seen = Arc::new(Mutex::new(Vec::<(Value, String)>::new()));
    let route_seen = Arc::clone(&seen);
    let router = Router::new().route(
        "/verify",
        post(move |headers: AxumHeaderMap, Json(body): Json<Value>| {
            let route_seen = Arc::clone(&route_seen);
            async move {
                route_seen.lock().unwrap().push((
                    body,
                    headers
                        .get("idempotency-key")
                        .and_then(|value| value.to_str().ok())
                        .unwrap_or_default()
                        .to_owned(),
                ));
                Json(json!({"isValid": true, "payer": PAYER}))
            }
        }),
    );
    let (base_url, server) = spawn(router).await;
    let provider = provider(&base_url, 16 * 1024);
    let request = request();

    let result = provider.execute(SettlementOperation::Verify, &request).await.unwrap();

    assert!(result.valid);
    assert!(!result.settled);
    assert_eq!(result.asset, Some(expected_asset()));
    assert_eq!(result.amount, Some(Amount::new(1_500_000)));
    assert_eq!(result.recipient.as_deref(), Some(PAY_TO));
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].1, "intent-1:verify");
    let object = seen[0].0.as_object().unwrap();
    assert_eq!(object.len(), 3);
    assert_eq!(object.get("x402Version"), Some(&json!(2)));
    assert_eq!(object.get("paymentPayload"), Some(&request.payment_payload));
    assert_eq!(object.get("paymentRequirements"), Some(&request.payment_requirements));
    server.abort();
}

#[tokio::test]
async fn rejects_a_success_response_for_the_wrong_network() {
    let router = Router::new().route(
        "/settle",
        post(|| async {
            Json(json!({
                "success": true,
                "payer": PAYER,
                "transaction": transaction(),
                "network": "eip155:1"
            }))
        }),
    );
    let (base_url, server) = spawn(router).await;
    let error = provider(&base_url, 16 * 1024)
        .execute(SettlementOperation::Settle, &request())
        .await
        .unwrap_err();
    assert!(matches!(error, Error::Settlement(_)));
    server.abort();
}

#[tokio::test]
async fn rejects_an_unregistered_payment_requirements_asset_before_http() {
    let provider = provider("http://127.0.0.1:9/", 16 * 1024);
    let mut request = request();
    request.payment_requirements["asset"] = Value::String("0x0000000000000000000000000000000000009999".to_owned());
    request.payment_payload["accepted"] = request.payment_requirements.clone();

    let error = provider
        .execute(SettlementOperation::Verify, &request)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("not registered"));
}

#[tokio::test]
async fn rejects_oversized_facilitator_responses() {
    let router = Router::new().route(
        "/verify",
        post(|| async {
            Response::builder()
                .header("content-type", "application/json")
                .body(Body::from(format!(
                    r#"{{"isValid":false,"invalidReason":"{}"}}"#,
                    "x".repeat(512)
                )))
                .unwrap()
        }),
    );
    let (base_url, server) = spawn(router).await;
    let error = provider(&base_url, 64)
        .execute(SettlementOperation::Verify, &request())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("exceeds configured limit"));
    server.abort();
}

#[tokio::test]
async fn exposes_only_a_bounded_safe_facilitator_reason() {
    let router = Router::new().route(
        "/verify",
        post(|| async {
            Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .header("content-type", "application/json")
                .body(Body::from(
                    r#"{"invalidReason":"onchain_failure","invalidReasonDetails":"signature=0xdeadbeef"}"#,
                ))
                .unwrap()
        }),
    );
    let (base_url, server) = spawn(router).await;
    let error = provider(&base_url, 16 * 1024)
        .execute(SettlementOperation::Verify, &request())
        .await
        .unwrap_err()
        .to_string();

    assert!(error.contains("onchain_failure"));
    assert!(!error.contains("deadbeef"));
    assert!(!error.contains("signature"));
    server.abort();
}

#[tokio::test]
async fn never_follows_facilitator_redirects() {
    let target_hits = Arc::new(AtomicUsize::new(0));
    let route_hits = Arc::clone(&target_hits);
    let router = Router::new()
        .route("/verify", post(|| async { Redirect::temporary("/credential-sink") }))
        .route(
            "/credential-sink",
            post(move || {
                let route_hits = Arc::clone(&route_hits);
                async move {
                    route_hits.fetch_add(1, Ordering::SeqCst);
                    Json(json!({"isValid": true, "payer": PAYER}))
                }
            }),
        );
    let (base_url, server) = spawn(router).await;
    let error = provider(&base_url, 16 * 1024)
        .execute(SettlementOperation::Verify, &request())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("307"));
    assert_eq!(target_hits.load(Ordering::SeqCst), 0);
    server.abort();
}

#[tokio::test]
async fn maps_successful_settlement_to_a_stable_effect() {
    let router = Router::new().route(
        "/settle",
        post(|| async {
            Json(json!({
                "success": true,
                "payer": PAYER,
                "transaction": transaction(),
                "network": "eip155:8453"
            }))
        }),
    );
    let (base_url, server) = spawn(router).await;
    let provider = provider(&base_url, 16 * 1024);
    let first = provider.execute(SettlementOperation::Settle, &request()).await.unwrap();
    let second = provider.execute(SettlementOperation::Settle, &request()).await.unwrap();
    assert!(first.valid && first.settled);
    assert_eq!(first.effect_ref, second.effect_ref);
    assert!(first
        .effect_ref
        .as_deref()
        .is_some_and(|effect| effect.starts_with("x402-exact:")));
    assert_eq!(first.confirmations, Some(1));
    server.abort();
}

#[tokio::test]
async fn rejects_nonstandard_settlement_response_fields() {
    let router = Router::new().route(
        "/settle",
        post(|| async {
            Json(json!({
                "success": true,
                "payer": PAYER,
                "transaction": transaction(),
                "network": "eip155:8453",
                "amount": "1500000"
            }))
        }),
    );
    let (base_url, server) = spawn(router).await;
    let error = provider(&base_url, 16 * 1024)
        .execute(SettlementOperation::Settle, &request())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("official wire variant"));
    server.abort();
}

#[tokio::test]
async fn rejects_inconsistent_verify_success_fields() {
    let router = Router::new().route(
        "/verify",
        post(|| async { Json(json!({"isValid": true, "invalidReason": "bad"})) }),
    );
    let (base_url, server) = spawn(router).await;
    assert!(provider(&base_url, 16 * 1024)
        .execute(SettlementOperation::Verify, &request())
        .await
        .is_err());
    server.abort();
}

fn provider(base_url: &str, max_response_bytes: usize) -> X402FacilitatorProvider {
    X402FacilitatorProvider::new(
        &SettlementProviderConfig {
            base_url: base_url.to_owned(),
            timeout: Duration::from_secs(2),
            max_response_bytes,
            allow_insecure_loopback: true,
            ..SettlementProviderConfig::default()
        },
        HeaderMap::new(),
        vec![X402Asset {
            network: "eip155:8453".to_owned(),
            contract: CONTRACT.to_owned(),
            symbol: "USDC".to_owned(),
            decimals: 6,
        }],
    )
    .unwrap()
}

fn request() -> SettlementProviderRequest {
    let requirements = json!({
        "scheme": "exact",
        "network": "eip155:8453",
        "amount": "1500000",
        "asset": CONTRACT,
        "payTo": PAY_TO,
        "maxTimeoutSeconds": 300,
        "extra": {"name": "USDC", "version": "2"}
    });
    SettlementProviderRequest {
        protocol: "x402".to_owned(),
        idempotency_key: "intent-1:verify".to_owned(),
        payment_payload: json!({
            "x402Version": 2,
            "accepted": requirements,
            "payload": {
                "signature": format!("0x{}", "11".repeat(65)),
                "authorization": {
                    "from": PAYER,
                    "to": PAY_TO,
                    "value": "1500000",
                    "validAfter": "1",
                    "validBefore": "9999999999",
                    "nonce": format!("0x{}", "22".repeat(32))
                }
            }
        }),
        payment_requirements: requirements,
    }
}

fn expected_asset() -> AssetIdentity {
    AssetIdentity {
        network: "eip155:8453".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: Some(CONTRACT.to_owned()),
        decimals: 6,
    }
}

fn transaction() -> String {
    format!("0x{}", "ab".repeat(32))
}

async fn spawn(router: Router) -> (String, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    (format!("http://{address}/"), server)
}
