use crate::storage::schema_tests::RepositoryFixture;

impl RepositoryFixture {
    pub async fn event_contract(self) {
        use crate::{
            model::{PaymentEvent, PaymentEventType},
            storage::{IdempotentWrite, PageRequest},
        };
        let intent = self.challenge().await;
        let event = PaymentEvent {
            id: format!("retry-{}", intent.id),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::Retried,
            attempt_no: 1,
            signature_ref: None,
            tx_hash: None,
            payload: serde_json::json!({"stage":"verify"}),
            created_at: 11,
        };
        assert!(matches!(
            self.ports.events.append(&event).await.unwrap(),
            IdempotentWrite::Inserted(_)
        ));
        assert_eq!(
            self.ports.events.append(&event).await.unwrap(),
            IdempotentWrite::Replay(event.clone())
        );
        let mut altered = event.clone();
        altered.payload = serde_json::json!({"stage":"settle"});
        assert!(self.ports.events.append(&altered).await.is_err());
        assert_eq!(self.ports.events.get(&event.id).await.unwrap(), Some(event));
        assert_eq!(
            self.ports
                .events
                .list_for_intent(&intent.id, PageRequest::default())
                .await
                .unwrap()
                .total,
            2
        );
    }
}
