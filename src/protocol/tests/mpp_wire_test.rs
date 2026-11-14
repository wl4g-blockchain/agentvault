use super::*;

#[test]
fn mpp_reference_requires_canonical_nonempty_hex() {
    assert_eq!(MppWire::decode_payment_reference("0x1234").unwrap(), [0x12, 0x34]);
    for value in ["0x", "0x1", "1234", "0xZZ"] {
        assert!(MppWire::decode_payment_reference(value).is_err());
    }
    assert!(MppWire::valid_transaction_hash(&format!("0x{}", "11".repeat(32))));
    assert!(!MppWire::valid_transaction_hash("0x1234"));
}

#[test]
fn tempo_network_uses_the_upstream_numeric_chain_id_contract() {
    let mut request = mpp::protocol::intents::ChargeRequest {
        method_details: Some(serde_json::json!({"chainId":42431})),
        ..Default::default()
    };
    assert_eq!(MppWire::tempo_network(&request).unwrap(), "eip155:42431");
    for value in [
        serde_json::json!("42431"),
        serde_json::json!(-1),
        serde_json::json!(1.5),
    ] {
        request.method_details = Some(serde_json::json!({"chainId":value}));
        assert!(MppWire::tempo_network(&request).is_err());
    }
}
