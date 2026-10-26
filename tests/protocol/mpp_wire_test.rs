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
