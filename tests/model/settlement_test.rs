use super::*;

#[test]
fn provider_result_accepts_only_declared_wire_aliases() {
    let result: SettlementProviderResult = serde_json::from_value(serde_json::json!({
        "isValid": true,
        "success": true,
        "transactionHash": "0x01"
    }))
    .unwrap();

    assert!(result.valid);
    assert!(result.settled);
    assert_eq!(result.transaction.as_deref(), Some("0x01"));
}
