use super::*;

#[tokio::test]
async fn unconfigured_chain_never_reads_facilitator_custody() {
    std::env::remove_var("SOLANA_RPC_URL");
    std::env::remove_var("SOLANA_FACILITATOR_KEYPAIR");
    assert!(SolanaFacilitator::from_env().await.unwrap().is_none());
}

#[test]
fn failure_responses_keep_verify_and_settlement_wire_variants_separate() {
    assert_eq!(
        SolanaFacilitator::rejection(false, "eip155:31337"),
        json!({"isValid": false, "invalidReason": "payment_rejected"})
    );
    assert_eq!(
        SolanaFacilitator::rejection(true, "eip155:31337"),
        json!({"success": false, "errorReason": "payment_rejected", "network": "eip155:31337"})
    );
}

#[test]
fn upstream_error_classification_never_returns_request_material() {
    for (body, expected) in [
        (
            r#"{"error":"Contract call failed: execution reverted"}"#,
            "onchain_call_failed",
        ),
        (
            r#"{"error":"Invalid format: missing field `accepted`"}"#,
            "invalid_wire_format",
        ),
        (
            r#"{"error":"Onchain balance is not enough to cover the payment amount"}"#,
            "insufficient_funds",
        ),
        (r#"{"error":"authorization not active"}"#, "authorization_not_active"),
        (
            r#"{"error":"Invalid signature", "signature":"private-material"}"#,
            "invalid_signature",
        ),
        (r#"{"credential":"private-material"}"#, "unclassified_upstream_error"),
    ] {
        assert_eq!(SolanaFacilitator::upstream_error_class(body.as_bytes()), expected);
    }
}
