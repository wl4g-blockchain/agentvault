use super::*;
#[test]
fn provider_registry_rejects_empty_trusted_confirmation_policy() {
    let config = SettlementProviderConfig {
        base_url: "https://provider.test".into(),
        ..Default::default()
    };
    let provider = Arc::new(HttpSettlementProvider::new(&config, HeaderMap::new()).unwrap());
    assert!(SettlementProviderRegistry::new([(
        "mpp".into(),
        "provider".into(),
        BTreeMap::new(),
        provider as Arc<dyn ExternalSettlementProvider>
    )])
    .is_err());
}
