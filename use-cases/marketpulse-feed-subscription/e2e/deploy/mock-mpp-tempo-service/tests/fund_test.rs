use super::*;

#[test]
fn faucet_rejects_amounts_or_transactions_supplied_by_callers() {
    assert!(serde_json::from_value::<FundingRequest>(json!({
        "address": "0x1111111111111111111111111111111111111111", "amount": "1"
    }))
    .is_err());
    assert!(serde_json::from_value::<FundingRequest>(json!({
        "address": "0x1111111111111111111111111111111111111111", "transaction": "0x01"
    }))
    .is_err());
}
