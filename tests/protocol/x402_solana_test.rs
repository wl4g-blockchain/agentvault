use super::*;

#[test]
fn solana_wire_rejects_invalid_transactions_and_evm_payloads() {
    assert!(X402Solana::decode_transaction("not-base64!").is_err());
    assert!(X402Solana::payment_payer(&json!({"payload":{"signature":"0x00"}})).is_err());
    assert!(X402Solana::valid_network("solana:4uhcVJyU9pJkvQyS88uRDiswHXSCkY3z"));
    assert!(!X402Solana::valid_network("eip155:31337"));
}
