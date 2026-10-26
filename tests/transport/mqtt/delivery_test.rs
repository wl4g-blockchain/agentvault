use super::*;
use crate::storage::OutboxStatus;
#[test]
fn maps_only_approval_required_outbox_destinations() {
    let message = OutboxMessage {
        id: "outbox-1".to_owned(),
        aggregate_type: "payment_intent".to_owned(),
        aggregate_id: "intent-1".to_owned(),
        event_type: "approval_required".to_owned(),
        destination: "client:client-quantstream.approvals".to_owned(),
        run_id: "approval-run-1".to_owned(),
        payload: json!({
            "approval_id": "approval-1",
            "intent_id": "intent-1",
            "run_id": "approval-run-1",
            "scenario_id": "scenario-quantstream",
            "amount": "50000000",
            "expires_at": 120
        }),
        status: OutboxStatus::Pending,
        attempt_count: 1,
        available_at: 10,
        lease_owner: Some("mqtt:agentvault".to_owned()),
        lease_expires_at: Some(40),
        last_error: None,
        created_at: 10,
        delivered_at: None,
    };
    assert_eq!(
        MqttOutboxPublisher::approval_destination_client(&message).as_deref(),
        Some("client-quantstream")
    );
    let envelope = serde_json::to_value(MqttOutboxPublisher::approval_envelope(&message).unwrap()).unwrap();
    assert_eq!(envelope["type"], "payment.approval_required");
    assert_eq!(envelope["runId"], "approval-run-1");
    assert_eq!(envelope["scenarioId"], "scenario-quantstream");
    assert_eq!(envelope["requestedAmount"], "50000000");

    let mut mismatched = message.clone();
    mismatched.payload["run_id"] = Value::String("another-run".to_owned());
    assert!(MqttOutboxPublisher::approval_envelope(&mismatched).is_err());

    let mut foreign = message;
    foreign.event_type = "payment_settled".to_owned();
    foreign.destination = "scenario:scenario-1.payments".to_owned();
    assert!(MqttOutboxPublisher::approval_destination_client(&foreign).is_none());
}

#[test]
fn payment_event_routing_is_tenant_and_run_scoped() {
    let message = OutboxMessage {
        id: "outbox-event-1".to_owned(),
        aggregate_type: "payment_intent".to_owned(),
        aggregate_id: "intent-1".to_owned(),
        event_type: "payment_settled".to_owned(),
        destination: "scenario:scenario-quantstream.payments".to_owned(),
        run_id: "run-42".to_owned(),
        payload: json!({"intent_id": "intent-1", "tx_hash": "0x1234"}),
        status: OutboxStatus::Pending,
        attempt_count: 1,
        available_at: 10,
        lease_owner: Some("mqtt:agentvault:payment-events".to_owned()),
        lease_expires_at: Some(40),
        last_error: None,
        created_at: 10,
        delivered_at: None,
    };
    assert!(matches!(
        MqttPaymentEventSink::payment_event_destination(&message.destination),
        Ok(PaymentEventDestination::Scenario("scenario-quantstream"))
    ));
    assert!(matches!(
        MqttPaymentEventSink::payment_event_destination("client:client-quantstream.payments"),
        Ok(PaymentEventDestination::Client("client-quantstream"))
    ));
    assert!(MqttPaymentEventSink::payment_event_destination("approval").is_err());
    assert!(MqttPaymentEventSink::payment_event_destination("scenario:../client.payments").is_err());

    let envelope = serde_json::to_value(MqttPaymentEventSink::payment_event_envelope(&message).unwrap()).unwrap();
    assert_eq!(envelope["type"], "payment.event");
    assert_eq!(envelope["runId"], "run-42");
    assert_eq!(envelope["eventType"], "payment_settled");
}
