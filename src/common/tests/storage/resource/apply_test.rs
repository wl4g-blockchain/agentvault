use super::*;
#[test]
fn external_wallet_material_never_acquires_a_custody_private_key() {
    let spec = WalletSpec {
        id: "wallet".into(),
        enabled: true,
        budget_unit: None,
        max_per_payment: None,
        max_per_day: None,
        source: WalletSource::External {
            algorithm: "external".into(),
            address: "recipient".into(),
            public_key: String::new(),
        },
    };
    let (wallet, key) = ResourceImporter::wallet_material(&spec, 123).unwrap();
    assert_eq!(wallet.address, "recipient");
    assert!(key.is_none());
}
