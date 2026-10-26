use super::*;

#[test]
fn delivery_classes_require_a_scoped_destination_and_never_share_approval_rows() {
    let mut message = OutboxMessage {
        id: "outbox".into(),
        aggregate_type: "payment".into(),
        aggregate_id: "intent".into(),
        event_type: "approval_required".into(),
        destination: "client:workload.approvals".into(),
        run_id: "run".into(),
        payload: serde_json::json!({}),
        status: OutboxStatus::Pending,
        attempt_count: 0,
        available_at: 1,
        lease_owner: None,
        lease_expires_at: None,
        last_error: None,
        created_at: 1,
        delivered_at: None,
    };
    assert!(OutboxDeliveryClass::ApprovalRequired.matches(&message));
    assert!(!OutboxDeliveryClass::PaymentEvents.matches(&message));
    message.destination = "client:.approvals".into();
    assert!(!OutboxDeliveryClass::ApprovalRequired.matches(&message));
    message.destination = "scenario:feed.payments".into();
    assert!(OutboxDeliveryClass::PaymentEvents.matches(&message));
    assert!(!OutboxDeliveryClass::ApprovalRequired.matches(&message));
}
