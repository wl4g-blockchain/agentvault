use super::*;
use crate::payment::HttpSettlementProvider;
use crate::payment::SettlementProviderConfig;
use reqwest::header::HeaderMap;

#[test]
fn rejects_remote_plain_http_and_path_traversal() {
    let remote = SettlementProviderConfig {
        base_url: "http://provider.example".to_owned(),
        ..SettlementProviderConfig::default()
    };
    assert!(HttpSettlementProvider::new(&remote, HeaderMap::new()).is_err());

    let traversal = SettlementProviderConfig {
        base_url: "https://provider.example/api".to_owned(),
        settle_path: "../settle".to_owned(),
        ..SettlementProviderConfig::default()
    };
    assert!(HttpSettlementProvider::new(&traversal, HeaderMap::new()).is_err());

    let absolute_operation = SettlementProviderConfig {
        base_url: "https://provider.example/api".to_owned(),
        verify_path: "https://evil.example/verify".to_owned(),
        ..SettlementProviderConfig::default()
    };
    assert!(HttpSettlementProvider::new(&absolute_operation, HeaderMap::new()).is_err());

    let encoded_traversal = SettlementProviderConfig {
        base_url: "https://provider.example/api".to_owned(),
        settle_path: "%2e%2e/settle".to_owned(),
        ..SettlementProviderConfig::default()
    };
    assert!(HttpSettlementProvider::new(&encoded_traversal, HeaderMap::new()).is_err());
}

#[test]
fn permits_explicit_loopback_development_provider() {
    let config = SettlementProviderConfig {
        base_url: "http://127.0.0.1:8088/api".to_owned(),
        allow_insecure_loopback: true,
        ..SettlementProviderConfig::default()
    };
    assert!(HttpSettlementProvider::new(&config, HeaderMap::new()).is_ok());
}

#[test]
fn accepts_the_generic_provider_result_contract() {
    let result: SettlementProviderResult = serde_json::from_value(json!({
        "valid": true,
        "settled": true,
        "transaction": "0xabc",
        "settlementRef": "provider-1"
    }))
    .unwrap();

    assert!(result.valid);
    assert!(result.settled);
    assert_eq!(result.transaction.as_deref(), Some("0xabc"));
    assert_eq!(result.settlement_ref.as_deref(), Some("provider-1"));
}
