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

#[test]
fn durable_receipt_falls_back_to_reconciliation_event() {
    let receipt = PaymentReceipt {
        intent_id: "intent-1".to_owned(),
        scenario_id: "scenario-1".to_owned(),
        direction: PaymentDirection::Inbound,
        protocol: "x402".to_owned(),
        asset: AssetIdentity {
            network: "eip155:31337".to_owned(),
            asset: "USDC".to_owned(),
            token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
            decimals: 6,
        },
        recipient: "0x2222222222222222222222222222222222222222".to_owned(),
        payer: Some("0x3333333333333333333333333333333333333333".to_owned()),
        requested_amount: Amount::new(10),
        settled_amount: Amount::new(10),
        status: PaymentStatus::Settled,
        tx_hash: Some(format!("0x{}", "ab".repeat(32))),
        settlement_ref: None,
        evidence_hash: "evidence".to_owned(),
        confirmed_at: 10,
    };
    let reconciled = PaymentEvent {
        id: "event-1".to_owned(),
        intent_id: receipt.intent_id.clone(),
        event_type: PaymentEventType::Reconciled,
        attempt_no: 0,
        signature_ref: None,
        tx_hash: receipt.tx_hash.clone(),
        payload: json!({"receipt": receipt}),
        created_at: 10,
    };

    let recovered = durable_receipt(None, Some(&reconciled)).unwrap();
    assert_eq!(recovered.intent_id, "intent-1");
    assert_eq!(recovered.status, PaymentStatus::Settled);
}
