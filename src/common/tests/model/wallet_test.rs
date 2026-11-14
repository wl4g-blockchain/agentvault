use super::*;

#[test]
fn canonicalizes_and_validates_evm_addresses_by_algorithm() {
    let mixed = "0XAbCdEf0123456789aBCDef0123456789AbCDef01";
    assert_eq!(
        canonicalize_wallet_address("SECP256K1", mixed).unwrap(),
        "0xabcdef0123456789abcdef0123456789abcdef01"
    );
    for invalid in [
        "abcdef0123456789abcdef0123456789abcdef01",
        "0xabcdef",
        "0xgggggggggggggggggggggggggggggggggggggggg",
    ] {
        assert_eq!(canonicalize_wallet_address(EVM_EOA_ALGORITHM, invalid), Err("address"));
    }
}

#[test]
fn preserves_non_evm_address_semantics() {
    let opaque = "MixedCase:Not-An-EVM-Address";
    assert_eq!(canonicalize_wallet_address("external", opaque).unwrap(), opaque);
    assert!(!wallet_addresses_match("external", "CaseSensitive", "casesensitive"));
}

#[test]
fn canonicalizes_strict_external_evm_address_without_conflating_custody() {
    let mixed = "0XAbCdEf0123456789aBCDef0123456789AbCDef01";
    let lower = "0xabcdef0123456789abcdef0123456789abcdef01";
    assert_eq!(canonicalize_wallet_address("external", mixed).unwrap(), lower);
    assert!(wallet_addresses_match("external", mixed, lower));
    assert_eq!(
        canonicalize_wallet_address("external", "0xnot-an-address").unwrap(),
        "0xnot-an-address"
    );
}

#[test]
fn matches_evm_recipients_only_on_eip155_networks() {
    let lower = "0xabcdef0123456789abcdef0123456789abcdef01";
    let mixed = "0xAbCdEf0123456789aBCDef0123456789AbCDef01";
    assert!(payment_recipients_match("eip155:8453", lower, mixed));
    assert!(!payment_recipients_match("tempo:mainnet", lower, mixed));
    assert!(!payment_recipients_match("eip155:8453", "0xabc", "0xAbC"));
    assert_eq!(canonicalize_payment_recipient("eip155:8453", "opaque"), Err("address"));
}
