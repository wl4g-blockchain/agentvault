use super::*;

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
fn accepts_x402_provider_result_names() {
    let result: SettlementProviderResult = serde_json::from_value(json!({
        "isValid": true,
        "success": true,
        "transactionHash": "0xabc",
        "settlementRef": "provider-1"
    }))
    .unwrap();

    assert!(result.valid);
    assert!(result.settled);
    assert_eq!(result.transaction.as_deref(), Some("0xabc"));
    assert_eq!(result.settlement_ref.as_deref(), Some("provider-1"));
}

#[test]
fn evidence_hash_is_stable_and_domain_separated() {
    let evidence = json!({"tx": "0xabc", "amount": "100"});
    assert_eq!(evidence_hash(&evidence).unwrap(), evidence_hash(&evidence).unwrap());
    assert_ne!(
        evidence_hash(&evidence).unwrap(),
        evidence_hash(&json!({"tx": "0xdef"})).unwrap()
    );
}

#[test]
fn authorization_expiry_requires_both_wall_clock_and_scanned_chain_time() {
    assert!(!authorization_is_definitively_expired(100, 99, 100));
    assert!(!authorization_is_definitively_expired(99, 100, 100));
    assert!(authorization_is_definitively_expired(100, 100, 100));
}

#[test]
fn terminal_non_settlement_releases_only_an_outstanding_reservation() {
    assert_eq!(
        reservation_after_non_settlement(PaymentStatus::Expired, ReservationStatus::Reserved),
        ReservationStatus::Released
    );
    assert_eq!(
        reservation_after_non_settlement(PaymentStatus::Failed, ReservationStatus::Reserved),
        ReservationStatus::Released
    );
    assert_eq!(
        reservation_after_non_settlement(PaymentStatus::Unknown, ReservationStatus::Reserved),
        ReservationStatus::Reserved
    );
    assert_eq!(
        reservation_after_non_settlement(PaymentStatus::Expired, ReservationStatus::Consumed),
        ReservationStatus::Consumed
    );
}
