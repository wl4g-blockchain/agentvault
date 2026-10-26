use crate::storage::outbox::postgres::OutboxPostgresRepository;
use crate::storage::postgres::tests::*;
use crate::storage::IOutboxRepository as _;

#[tokio::test]
#[ignore = "requires AGENTVAULT_TEST_POSTGRES_URL pointing at a dedicated PostgreSQL database"]
async fn outbox_class_claim_isolated_and_replayed_after_worker_restart() {
    let database = storage().await;
    let approval_id = unique("pg-approval-outbox");
    let payment_id = unique("pg-payment-outbox");
    let foreign_id = unique("pg-foreign-outbox");
    for (id, event_type, destination) in [
        (approval_id.as_str(), "approval_required", "client:client-1.approvals"),
        (payment_id.as_str(), "payment_settled", "scenario:scenario-1.payments"),
        (foreign_id.as_str(), "webhook_event", "webhook:external"),
    ] {
        sqlx::query(
            "INSERT INTO pay_outbox (id, aggregate_type, aggregate_id, event_type, destination, \
             run_id, payload, status, attempt_count, available_at, created_at) \
             VALUES ($1, 'payment_intent', 'intent-1', $2, $3, $1, '{}'::jsonb, 'pending', 0, 10, 10)",
        )
        .bind(id)
        .bind(event_type)
        .bind(destination)
        .execute(database.pool())
        .await
        .unwrap();
    }

    let approval = OutboxPostgresRepository::new(database.clone())
        .claim(&crate::storage::OutboxLeaseRequest {
            owner: "pg-approval-worker".to_owned(),
            class: crate::storage::OutboxDeliveryClass::ApprovalRequired,
            now: 10,
            lease_until: 20,
            limit: 100,
        })
        .await
        .unwrap();
    assert!(approval.iter().any(|message| message.id == approval_id));
    assert!(!approval.iter().any(|message| message.id == payment_id));
    let untouched: (i64, Option<String>) =
        sqlx::query_as("SELECT attempt_count, lease_owner FROM pay_outbox WHERE id = $1")
            .bind(&payment_id)
            .fetch_one(database.pool())
            .await
            .unwrap();
    assert_eq!(untouched, (0, None));

    drop(database);
    let reopened = storage().await;
    let payment = OutboxPostgresRepository::new(reopened.clone())
        .claim(&crate::storage::OutboxLeaseRequest {
            owner: "pg-payment-worker".to_owned(),
            class: crate::storage::OutboxDeliveryClass::PaymentEvents,
            now: 21,
            lease_until: 31,
            limit: 100,
        })
        .await
        .unwrap();
    assert!(payment.iter().any(|message| message.id == payment_id));
    assert!(!payment.iter().any(|message| message.id == approval_id));
    let replay = OutboxPostgresRepository::new(reopened.clone())
        .claim(&crate::storage::OutboxLeaseRequest {
            owner: "pg-approval-worker-restarted".to_owned(),
            class: crate::storage::OutboxDeliveryClass::ApprovalRequired,
            now: 21,
            lease_until: 31,
            limit: 100,
        })
        .await
        .unwrap();
    let replay = replay.iter().find(|message| message.id == approval_id).unwrap();
    assert_eq!(replay.attempt_count, 2);
    let foreign: (i64, Option<String>) =
        sqlx::query_as("SELECT attempt_count, lease_owner FROM pay_outbox WHERE id = $1")
            .bind(&foreign_id)
            .fetch_one(reopened.pool())
            .await
            .unwrap();
    assert_eq!(foreign, (0, None));
}
