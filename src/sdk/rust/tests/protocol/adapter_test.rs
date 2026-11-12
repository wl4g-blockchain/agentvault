use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use serde_json::{json, Value};

use crate::{
    HttpCredential, PaymentChallenge, PaymentCredential, PaymentProtocol, ProtocolHttpResponse, SettledIncomingPayment,
};

use super::*;

struct TestAdapter;

#[test]
fn shared_fixture_covers_both_transports_protocols_and_roles() {
    let fixture: Value =
        serde_json::from_str(&std::fs::read_to_string("../tests/payment_protocol_http.json").expect("fixture"))
            .expect("fixture JSON");
    assert_eq!(fixture["schema"], "agentvault.sdk.protocol-http.v1");
    let registry = PaymentProtocolFactory::new()
        .create(&["x402".to_owned(), "mpp".to_owned()])
        .expect("registry");
    let mut combined_evidence = BTreeMap::new();
    for name in ["x402", "mpp"] {
        let evidence_header = fixture["protocols"][name]["evidence_header"]
            .as_str()
            .expect("evidence header");
        let evidence_value = match name {
            "x402" => {
                let value = serde_json::to_vec(&x402_types::proto::v2::SettleResponse::Success {
                    payer: "0x2222222222222222222222222222222222222222".to_owned(),
                    transaction: "0x1111".to_owned(),
                    network: "eip155:42431".to_owned(),
                })
                .expect("x402 evidence");
                x402_types::util::b64::Base64Bytes::encode(value).to_string()
            }
            "mpp" => mpp::format_receipt(&mpp::Receipt::success("tempo", "0x1111")).expect("MPP evidence"),
            _ => unreachable!(),
        };
        let evidence = registry
            .get(name)
            .expect("protocol")
            .merchant_evidence(&BTreeMap::from([(evidence_header.to_owned(), evidence_value.clone())]))
            .expect("evidence")
            .expect("present evidence");
        assert_eq!(evidence.header_name, evidence_header);
        combined_evidence.insert(evidence_header.to_owned(), evidence_value);
    }
    assert!(registry.merchant_evidence(&combined_evidence).is_err());
    assert_eq!(
        registry.credential_header_names(),
        ["payment-signature", "payment-authorization", "authorization"]
    );
    assert_eq!(
        registry.merchant_evidence_header_names(),
        ["payment-response", "payment-receipt"]
    );
    let expected = BTreeSet::from([
        "grpc/x402/payer",
        "grpc/x402/payee",
        "grpc/mpp/payer",
        "grpc/mpp/payee",
        "mqtt/x402/payer",
        "mqtt/x402/payee",
        "mqtt/mpp/payer",
        "mqtt/mpp/payee",
    ]);
    let matrix = fixture["matrix"].as_array().expect("matrix");
    let actual = matrix
        .iter()
        .map(|row| {
            format!(
                "{}/{}/{}",
                row["transport"].as_str().expect("transport"),
                row["protocol"].as_str().expect("protocol"),
                row["role"].as_str().expect("role")
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(matrix.len(), 8);
    assert_eq!(actual, expected.into_iter().map(str::to_owned).collect());
}

#[test]
fn shared_mcp_fixture_uses_protocol_native_models() {
    let fixture: Value =
        serde_json::from_str(&std::fs::read_to_string("../tests/payment_protocol_mcp.json").expect("MCP fixture"))
            .expect("MCP fixture JSON");
    assert_eq!(fixture["schema"], "agentvault.sdk.protocol-mcp.v1");
    let registry = PaymentProtocolFactory::new()
        .create(&["x402".to_owned(), "mpp".to_owned()])
        .expect("registry");
    let mut combined_credentials = serde_json::Map::new();
    let mut combined_receipts = serde_json::Map::new();
    for name in ["x402", "mpp"] {
        let contract = &fixture["protocols"][name];
        let (adapter, _) = registry
            .mcp_challenge(&contract["challenge_response"], 64 * 1024)
            .expect("MCP challenge")
            .expect("recognized MCP challenge");
        assert_eq!(adapter.name(), name);
        assert!(adapter
            .presented_mcp_credential(&contract["credential_meta"], 64 * 1024)
            .expect("MCP credential")
            .is_some());
        assert!(adapter
            .mcp_merchant_evidence(&contract["receipt_meta"], 64 * 1024)
            .expect("MCP receipt")
            .is_some());
        combined_credentials.extend(
            contract["credential_meta"]
                .as_object()
                .expect("credential metadata")
                .clone(),
        );
        combined_receipts.extend(contract["receipt_meta"].as_object().expect("receipt metadata").clone());
    }
    assert!(registry
        .presented_mcp_credential(&Value::Object(combined_credentials), 64 * 1024)
        .is_err());
    assert!(registry
        .mcp_merchant_evidence(&Value::Object(combined_receipts), 64 * 1024)
        .is_err());
    assert_eq!(
        registry.mcp_request_metadata_keys(),
        ["x402/payment", "org.paymentauth/credential"]
    );
    assert_eq!(
        registry.mcp_root_request_metadata_keys(),
        ["org.paymentauth/credential"]
    );
    assert_eq!(
        registry.mcp_receipt_metadata_keys(),
        ["x402/payment-response", "org.paymentauth/receipt"]
    );

    let expected = BTreeSet::from([
        "grpc/x402/payer",
        "grpc/x402/payee",
        "grpc/mpp/payer",
        "grpc/mpp/payee",
        "mqtt/x402/payer",
        "mqtt/x402/payee",
        "mqtt/mpp/payer",
        "mqtt/mpp/payee",
    ]);
    let matrix = fixture["matrix"].as_array().expect("MCP matrix");
    let actual = matrix
        .iter()
        .map(|row| {
            format!(
                "{}/{}/{}",
                row["transport"].as_str().expect("transport"),
                row["protocol"].as_str().expect("protocol"),
                row["role"].as_str().expect("role")
            )
        })
        .collect::<BTreeSet<_>>();
    assert_eq!(matrix.len(), 8);
    assert_eq!(actual, expected.into_iter().map(str::to_owned).collect());
}

impl PaymentProtocol for TestAdapter {
    fn name(&self) -> &'static str {
        "test"
    }

    fn credential(&self, _credential: &PaymentCredential) -> crate::Result<HttpCredential> {
        Err(crate::Error::InvalidArgument("unused".to_owned()))
    }

    fn merchant_evidence(&self, _headers: &BTreeMap<String, String>) -> crate::Result<Option<MerchantEvidence>> {
        Ok(None)
    }

    fn presented_credential(&self, _headers: &BTreeMap<String, String>, _limit: usize) -> crate::Result<Option<Value>> {
        Ok(None)
    }

    fn challenge(&self, _challenge: &PaymentChallenge) -> crate::Result<ProtocolHttpResponse> {
        Err(crate::Error::InvalidArgument("unused".to_owned()))
    }

    fn receipt(&self, _payment: &SettledIncomingPayment) -> crate::Result<ProtocolHttpResponse> {
        Err(crate::Error::InvalidArgument("unused".to_owned()))
    }
}

#[test]
fn factory_instances_are_isolated_and_reject_duplicates() {
    let first = PaymentProtocolFactory::new();
    first
        .register("test", || Arc::new(TestAdapter))
        .expect("register provider");
    assert!(first.create(&["test".to_owned()]).is_ok());

    let second = PaymentProtocolFactory::new();
    assert!(second.create(&["test".to_owned()]).is_err());
    assert!(second.create(&["x402".to_owned(), "x402".to_owned()]).is_err());
}

#[test]
fn mpp_preserves_bearer_and_never_uses_x402_wire_headers() {
    let registry = PaymentProtocolFactory::new()
        .create(&["x402".to_owned(), "mpp".to_owned()])
        .expect("registry");
    let mpp = registry.get("mpp").expect("mpp");
    let native = mpp::PaymentChallenge::new(
        "challenge",
        "merchant.example",
        "tempo",
        "charge",
        mpp::Base64UrlJson::from_value(&json!({"amount": "1000", "currency": "USDC"})).expect("MPP request"),
    )
    .with_header(mpp::PAYMENT_AUTHORIZATION_HEADER);
    let authorization = mpp::format_authorization(&mpp::PaymentCredential::new(
        native.to_echo(),
        mpp::PaymentPayload::hash("0x1111"),
    ))
    .expect("MPP authorization");
    let headers = BTreeMap::from([
        ("authorization".to_owned(), "Bearer app-token".to_owned()),
        ("payment-authorization".to_owned(), authorization.clone()),
    ]);
    assert_eq!(
        mpp.presented_credential(&headers, 1024).expect("credential"),
        Some(json!({"authorization": authorization}))
    );
    assert!(mpp
        .presented_credential(
            &BTreeMap::from([
                ("authorization".to_owned(), authorization.clone()),
                ("payment-authorization".to_owned(), authorization.clone()),
            ]),
            1024,
        )
        .is_err());

    let challenge: PaymentChallenge = serde_json::from_value(json!({
        "scenario_id": "scenario",
        "price_id": "price",
        "resource": "https://merchant.example/tool",
        "protocol": "mpp",
        "asset": {"network": "eip155:42431", "asset": "USDC", "decimals": 6},
        "amount": "1000",
        "recipient": "0x1111111111111111111111111111111111111111",
        "nonce": "nonce",
        "challenge_hash": "challenge",
        "expires_at": 2_000_000_000,
        "payload": {"www_authenticate": mpp::format_www_authenticate(&native).expect("MPP challenge")}
    }))
    .expect("challenge");
    let response = mpp.challenge(&challenge).expect("MPP response");
    assert!(response.headers.contains_key("www-authenticate"));
    assert!(!response.headers.contains_key("payment-required"));
}

#[test]
fn protocol_receipts_use_distinct_wire_formats() {
    let registry = PaymentProtocolFactory::new()
        .create(&["x402".to_owned(), "mpp".to_owned()])
        .expect("registry");
    let mut payment: SettledIncomingPayment = serde_json::from_value(json!({
        "intent": {
            "id": "intent", "scenario_id": "scenario", "run_id": "run",
            "wallet_id": "wallet", "policy_id": "policy", "price_id": "price",
            "direction": "inbound", "protocol": "mpp",
            "asset": {"network": "eip155:42431", "asset": "USDC", "decimals": 6},
            "recipient": "0x1111111111111111111111111111111111111111",
            "amount": "1000", "settled_amount": "1000",
            "resource": "https://merchant.example/tool", "challenge_hash": "challenge",
            "request_hash": "request", "status": "settled", "reservation_status": "consumed",
            "reserved_amount": "0", "tx_hash": "0x1111", "created_at": 1,
            "updated_at": 2, "settled_at": 2
        },
        "receipt": {
            "intent_id": "intent", "scenario_id": "scenario", "direction": "inbound",
            "protocol": "mpp",
            "asset": {"network": "eip155:42431", "asset": "USDC", "decimals": 6},
            "recipient": "0x1111111111111111111111111111111111111111",
            "payer": "0x2222222222222222222222222222222222222222",
            "requested_amount": "1000", "settled_amount": "1000", "status": "settled",
            "tx_hash": "0x1111", "evidence_hash": "evidence", "confirmed_at": 2_000_000_000
        }
    }))
    .expect("settled payment");
    let mpp = registry
        .get("mpp")
        .expect("mpp")
        .receipt(&payment)
        .expect("MPP receipt");
    assert!(mpp.headers.contains_key("payment-receipt"));
    assert!(!mpp.headers.contains_key("payment-response"));

    payment.intent.protocol = "x402".to_owned();
    payment.receipt.protocol = "x402".to_owned();
    let x402 = registry
        .get("x402")
        .expect("x402")
        .receipt(&payment)
        .expect("x402 receipt");
    assert!(x402.headers.contains_key("payment-response"));
    assert!(!x402.headers.contains_key("payment-receipt"));
}
