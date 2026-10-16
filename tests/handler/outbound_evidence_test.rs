use super::*;

fn encoded(value: &Value) -> String {
    Base64Bytes::encode(serde_json::to_vec(value).unwrap()).to_string()
}

#[test]
fn parses_only_the_official_settlement_response_wire() {
    let response = decode_payment_response(&encoded(&json!({
        "success": true,
        "transaction": format!("0x{}", "11".repeat(32)),
        "network": "eip155:8453",
        "payer": format!("0x{}", "22".repeat(20)),
    })))
    .unwrap();
    assert!(response.success);
    assert_eq!(response.network, "eip155:8453");

    assert!(decode_payment_response(&encoded(&json!({
        "valid": true,
        "transactionHash": format!("0x{}", "11".repeat(32)),
        "network": "eip155:8453",
    })))
    .is_err());
    assert!(decode_payment_response(&"A".repeat(MAX_PAYMENT_RESPONSE_BYTES + 1)).is_err());
}

#[test]
fn evidence_hash_binds_the_stable_operation_key() {
    let first = merchant_evidence_hash(
        "intent-1",
        "run-1:paid-feed",
        "eip155:8453",
        &format!("0x{}", "11".repeat(32)),
        &format!("0x{}", "22".repeat(20)),
    );
    let changed = merchant_evidence_hash(
        "intent-1",
        "different-key",
        "eip155:8453",
        &format!("0x{}", "11".repeat(32)),
        &format!("0x{}", "22".repeat(20)),
    );
    assert_ne!(first, changed);
}
