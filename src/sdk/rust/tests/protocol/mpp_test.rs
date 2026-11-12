use super::*;

#[test]
fn mpp_adapter_preserves_bearer_and_uses_only_native_headers() {
    let adapter = MppAdapter;
    let native = mpp::PaymentChallenge::new(
        "challenge",
        "merchant.example",
        "tempo",
        "charge",
        mpp::Base64UrlJson::from_value(&serde_json::json!({"amount": "1000", "currency": "USDC"})).expect("request"),
    )
    .with_header(mpp::PAYMENT_AUTHORIZATION_HEADER);
    let www_authenticate = mpp::format_www_authenticate(&native).expect("challenge header");
    let authorization = mpp::format_authorization(&mpp::PaymentCredential::new(
        native.to_echo(),
        mpp::PaymentPayload::hash("0x1111"),
    ))
    .expect("authorization");
    let challenge: PaymentChallenge = serde_json::from_value(serde_json::json!({
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
        "payload": {"www_authenticate": www_authenticate}
    }))
    .expect("challenge");
    let response = adapter.challenge(&challenge).expect("MPP response");
    assert!(response.headers.contains_key("www-authenticate"));
    assert!(!response.headers.contains_key("payment-required"));
    let credential = adapter
        .presented_credential(
            &BTreeMap::from([
                ("authorization".to_owned(), "Bearer app-token".to_owned()),
                ("payment-authorization".to_owned(), authorization.clone()),
            ]),
            1024,
        )
        .expect("credential");
    assert_eq!(credential, Some(serde_json::json!({"authorization": authorization})));
    assert!(adapter
        .presented_credential(
            &BTreeMap::from([("authorization".to_owned(), authorization.clone())]),
            1024,
        )
        .is_err());
    assert!(adapter
        .credential(&PaymentCredential {
            protocol: "mpp".to_owned(),
            challenge_hash: "challenge".to_owned(),
            credential_hash: "credential".to_owned(),
            payload: serde_json::json!({}),
            http: Some(HttpCredential {
                bound_origin: "https://merchant.example".to_owned(),
                header_name: "Authorization".to_owned(),
                header_value: authorization,
            }),
        })
        .is_err());
}

#[test]
fn mpp_adapter_uses_upstream_verification_failed_mcp_error() {
    let native = mpp::PaymentChallenge::new(
        "challenge-0123456789abcdef",
        "merchant.example",
        "tempo",
        "charge",
        mpp::Base64UrlJson::from_value(&serde_json::json!({"amount": "1000", "currency": "USDC"})).expect("request"),
    )
    .with_header(mpp::PAYMENT_AUTHORIZATION_HEADER);
    let challenge: PaymentChallenge = serde_json::from_value(serde_json::json!({
        "scenario_id": "scenario",
        "price_id": "price",
        "resource": "mcp://tool/marketpulse.realtime",
        "protocol": "mpp",
        "asset": {"network": "eip155:42431", "asset": "USDC", "decimals": 6},
        "amount": "1000",
        "recipient": "0x1111111111111111111111111111111111111111",
        "nonce": native.id.clone(),
        "challenge_hash": "challenge",
        "expires_at": 2_000_000_000,
        "payload": {"mcp_challenge": native}
    }))
    .expect("challenge");
    let ProtocolMcpPaymentRequired::Error(error) = MppAdapter
        .mcp_payment_rejected(&challenge, "AgentVault rejected the presented payment")
        .expect("rejection")
    else {
        panic!("MPP rejection must be a JSON-RPC error");
    };
    assert_eq!(error["code"], mpp::mcp::PAYMENT_VERIFICATION_FAILED_CODE);
    assert_eq!(error["data"]["httpStatus"], 402);
    assert_eq!(
        error["data"]["problem"]["type"],
        "https://paymentauth.org/problems/verification-failed"
    );
    assert_eq!(error["data"]["problem"]["challengeId"], "challenge-0123456789abcdef");
}
