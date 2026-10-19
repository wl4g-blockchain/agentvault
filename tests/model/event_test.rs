use super::*;

#[test]
fn event_type_and_identity_are_stable() {
    assert_eq!(PaymentEventType::SettlementRequested.as_str(), "settlement_requested");
    let event = PaymentEvent {
        id: "event-1".to_owned(),
        intent_id: "intent-1".to_owned(),
        event_type: PaymentEventType::Settled,
        attempt_no: 1,
        signature_ref: None,
        tx_hash: Some("0x1".to_owned()),
        payload: serde_json::json!({}),
        created_at: 1,
    };
    assert!(event.validate().is_ok());
}
