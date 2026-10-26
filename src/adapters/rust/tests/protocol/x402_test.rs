use super::*;

#[test]
fn x402_adapter_uses_only_native_x402_challenge_headers() {
    let challenge: PaymentChallenge = serde_json::from_value(serde_json::json!({
        "scenario_id": "scenario",
        "price_id": "price",
        "resource": "https://merchant.example/tool",
        "protocol": "x402",
        "asset": {"network": "eip155:42431", "asset": "USDC", "decimals": 6},
        "amount": "1000",
        "recipient": "0x1111111111111111111111111111111111111111",
        "nonce": "nonce",
        "challenge_hash": "challenge",
        "expires_at": 2_000_000_000,
        "payload": {"payment_required": {
            "x402Version": 2,
            "resource": {"url": "https://merchant.example/tool", "description": "tool", "mimeType": "application/json"},
            "accepts": [{
                "scheme": "exact",
                "network": "eip155:42431",
                "amount": "1000",
                "payTo": "0x1111111111111111111111111111111111111111",
                "maxTimeoutSeconds": 60,
                "asset": "0x2222222222222222222222222222222222222222",
                "extra": null
            }]
        }}
    }))
    .expect("challenge");
    let response = X402Adapter.challenge(&challenge).expect("x402 response");
    assert!(response.headers.contains_key("payment-required"));
    assert!(!response.headers.contains_key("www-authenticate"));
    let ProtocolMcpPaymentRequired::Result(rejected) = X402Adapter
        .mcp_payment_rejected(&challenge, "AgentVault rejected the presented payment")
        .expect("x402 rejection")
    else {
        panic!("x402 rejection must be an MCP tool result");
    };
    assert_eq!(
        rejected["structuredContent"]["error"],
        "AgentVault rejected the presented payment"
    );
}

#[test]
fn x402_adapter_does_not_misclassify_unrelated_mcp_tool_errors() {
    let message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "result": {
            "isError": true,
            "structuredContent": {"error": "permission denied"},
            "content": [{"type": "text", "text": "permission denied"}]
        }
    });
    assert!(X402Adapter
        .mcp_challenge(&message, 64 * 1024)
        .expect("unrelated tool error")
        .is_none());
}

#[test]
fn x402_adapter_preserves_chain_specific_v2_payloads_and_rejects_crossovers() {
    let fixture: Value = serde_json::from_str(include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../tests/x402_v2_chain_payloads.json"
    )))
    .expect("shared x402 fixture");
    for case in ["evm_eip3009", "solana_spl_exact"] {
        let payload = fixture["cases"][case]["payment_payload"].clone();
        let header = Base64Bytes::encode(serde_json::to_vec(&payload).expect("payment JSON")).to_string();
        let delivery = HttpCredential {
            bound_origin: "https://merchant.example".to_owned(),
            header_name: "PAYMENT-SIGNATURE".to_owned(),
            header_value: header.clone(),
        };
        let credential = PaymentCredential {
            protocol: "x402".to_owned(),
            challenge_hash: "challenge".to_owned(),
            credential_hash: "credential".to_owned(),
            payload: payload.clone(),
            http: Some(delivery.clone()),
        };

        assert_eq!(
            X402Adapter.credential(&credential).expect("payer HTTP credential"),
            delivery
        );
        let metadata = X402Adapter.mcp_credential(&credential).expect("payer MCP credential");
        assert_eq!(metadata.key, "x402/payment");
        assert_eq!(metadata.value, payload);

        assert_eq!(
            X402Adapter
                .presented_credential(&BTreeMap::from([("payment-signature".to_owned(), header)]), 64 * 1024,)
                .expect("payee HTTP credential")
                .expect("presented HTTP payload"),
            payload
        );
        assert_eq!(
            X402Adapter
                .presented_mcp_credential(&json!({"x402/payment": metadata.value}), 64 * 1024)
                .expect("payee MCP credential")
                .expect("presented MCP payload"),
            payload
        );
    }

    let mut crossover = fixture["cases"]["solana_spl_exact"]["payment_payload"].clone();
    crossover["accepted"] = fixture["cases"]["evm_eip3009"]["payment_payload"]["accepted"].clone();
    assert!(X402Adapter
        .presented_mcp_credential(&json!({"x402/payment": crossover}), 64 * 1024)
        .is_err());

    let mut expanded = fixture["cases"]["evm_eip3009"]["payment_payload"].clone();
    expanded["payload"]["unexpected"] = Value::Bool(true);
    assert!(X402Adapter
        .presented_mcp_credential(&json!({"x402/payment": expanded}), 64 * 1024)
        .is_err());
}
