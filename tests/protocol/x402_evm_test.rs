use super::*;

#[test]
fn evm_wire_rejects_other_schemes_and_wrong_address_families() {
    assert!(X402Evm::valid_network("eip155:31337"));
    assert!(!X402Evm::valid_network("solana:localnet"));
    assert!(X402Evm::valid_address("0x1111111111111111111111111111111111111111"));
    assert!(!X402Evm::valid_address("solana-payer"));
    assert!(X402Evm::digest(&json!({"x402Version":1})).is_err());
    assert!(X402Evm::verify_signature(&json!({})).is_err());
}
