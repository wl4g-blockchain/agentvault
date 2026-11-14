use super::*;

#[test]
fn entity_never_contains_custody_material() {
    let wallet = Wallet {
        id: "wallet-1".to_owned(),
        algorithm: "secp256k1".to_owned(),
        address: "0xAbCdEf0123456789aBCDef0123456789AbCDef01".to_owned(),
        public_key: "public-key".to_owned(),
        created_at: 1,
    }
    .canonicalized()
    .unwrap();

    assert_eq!(wallet.address, "0xabcdef0123456789abcdef0123456789abcdef01");
    assert!(!serde_json::to_string(&wallet).unwrap().contains("private"));
}
