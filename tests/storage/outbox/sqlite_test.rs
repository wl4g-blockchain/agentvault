use crate::config::SqliteStorageConfig;
use crate::storage::outbox::sqlite::OutboxSqliteRepository;
use crate::storage::sqlite::tests::*;
use crate::storage::sqlite::SqliteStorage;
use crate::storage::OutboxDeliveryClass;
use crate::storage::OutboxLeaseRequest;
use crate::storage::OutboxStorage as _;
use std::time::Duration;

#[tokio::test]
async fn outbox_class_claim_does_not_lease_foreign_rows_and_replays_after_restart() {
    let (directory, storage) = storage().await;
    let database_file = directory.path().join("agentvault.sqlite3").display().to_string();
    for (id, event_type, destination) in [
        ("approval-message", "approval_required", "client:client-1.approvals"),
        ("payment-message", "payment_settled", "scenario:scenario-1.payments"),
        ("foreign-message", "webhook_event", "webhook:external"),
    ] {
        sqlx::query(
            "INSERT INTO pay_outbox (id, aggregate_type, aggregate_id, event_type, destination, \
             run_id, payload, status, attempt_count, available_at, created_at) \
             VALUES (?1, 'payment_intent', 'intent-1', ?2, ?3, ?1, '{}', 'pending', 0, 10, 10)",
        )
        .bind(id)
        .bind(event_type)
        .bind(destination)
        .execute(storage.pool())
        .await
        .unwrap();
    }

    let approval = OutboxSqliteRepository::new(storage.clone())
        .claim(&OutboxLeaseRequest {
            owner: "approval-worker".to_owned(),
            class: OutboxDeliveryClass::ApprovalRequired,
            now: 10,
            lease_until: 20,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(
        approval.iter().map(|message| message.id.as_str()).collect::<Vec<_>>(),
        ["approval-message"]
    );
    let untouched: (i64, Option<String>) =
        sqlx::query_as("SELECT attempt_count, lease_owner FROM pay_outbox WHERE id = 'payment-message'")
            .fetch_one(storage.pool())
            .await
            .unwrap();
    assert_eq!(untouched, (0, None));

    drop(storage);
    let reopened = SqliteStorage::connect(&SqliteStorageConfig {
        database_file,
        max_connections: 4,
        busy_timeout: Duration::from_secs(5),
    })
    .await
    .unwrap();
    let payment = OutboxSqliteRepository::new(reopened.clone())
        .claim(&OutboxLeaseRequest {
            owner: "payment-worker".to_owned(),
            class: OutboxDeliveryClass::PaymentEvents,
            now: 21,
            lease_until: 31,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(
        payment.iter().map(|message| message.id.as_str()).collect::<Vec<_>>(),
        ["payment-message"]
    );
    let replay = OutboxSqliteRepository::new(reopened.clone())
        .claim(&OutboxLeaseRequest {
            owner: "approval-worker-restarted".to_owned(),
            class: OutboxDeliveryClass::ApprovalRequired,
            now: 21,
            lease_until: 31,
            limit: 10,
        })
        .await
        .unwrap();
    assert_eq!(replay.len(), 1);
    assert_eq!(replay[0].id, "approval-message");
    assert_eq!(replay[0].attempt_count, 2);
    let foreign: (i64, Option<String>) =
        sqlx::query_as("SELECT attempt_count, lease_owner FROM pay_outbox WHERE id = 'foreign-message'")
            .fetch_one(reopened.pool())
            .await
            .unwrap();
    assert_eq!(foreign, (0, None));
}
