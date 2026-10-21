use super::*;

#[test]
fn x402_adapter_uses_only_native_x402_challenge_headers() {
    let challenge: PaymentChallenge = serde_json::from_value(serde_json::json!({
        "scenario_id": "scenario",
        "offer_id": "offer",
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
