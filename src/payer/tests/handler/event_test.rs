use super::*;
use crate::model::PaymentEventType;

#[tokio::test]
async fn transition_event_replay_cannot_change_its_payload() {
    let (_directory, storage) = crate::storage::sqlite::tests::storage().await;
    let fixture = crate::storage::schema_tests::RepositoryFixture::create(std::sync::Arc::new(storage)).await;
    let intent = fixture.challenge().await;
    let service = EventHandler::new(fixture.ports.events);
    let event = PaymentEvent {
        id: "handler-event".into(),
        intent_id: intent.id,
        event_type: PaymentEventType::Retried,
        attempt_no: 1,
        signature_ref: None,
        tx_hash: None,
        payload: serde_json::json!({"stage":"verify"}),
        created_at: 11,
    };
    service.append(&event).await.unwrap();
    let mut conflict = event.clone();
    conflict.payload = serde_json::json!({"stage":"settle"});
    assert!(service.append(&conflict).await.is_err());
    assert_eq!(service.get(&event.id).await.unwrap(), Some(event));
}
