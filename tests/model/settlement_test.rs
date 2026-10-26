use super::*;

#[test]
fn provider_result_requires_the_canonical_wire_contract() {
    let result: SettlementProviderResult = serde_json::from_value(serde_json::json!({
        "valid": true, "settled": true, "transaction": "0x01"
    }))
    .unwrap();
    assert!(result.valid);
    assert!(result.settled);
    assert_eq!(result.transaction.as_deref(), Some("0x01"));
    for alias in ["isValid", "success", "transactionHash"] {
        let mut value = serde_json::json!({"valid":true,"settled":true});
        value[alias] = serde_json::json!(true);
        assert!(serde_json::from_value::<SettlementProviderResult>(value).is_err());
    }
}
