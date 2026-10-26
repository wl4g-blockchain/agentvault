use super::*;
#[test]
fn disabled_payee_registry_cannot_resolve_an_unconfigured_provider() {
    let registry = SettlementProviderRegistry::new([]).unwrap();
    assert!(registry.get("x402").is_err());
    assert!(registry.get("mpp").is_err());
}
