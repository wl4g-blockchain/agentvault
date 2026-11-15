use super::*;
#[test]
fn registry_rejects_unrecognized_or_ambiguous_network_identifiers() {
    for network in ["", "evm:8453", "eip155:not-a-number", "solana:invalid"] {
        assert!(ReceiptRegistry::validate_receipt_network(network).is_err());
    }
    assert!(ReceiptRegistry::validate_receipt_network("eip155:8453").is_ok());
}
