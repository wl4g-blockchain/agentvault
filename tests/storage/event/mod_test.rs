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
        self.event_attempt_contract(&intent.id).await;
    }

    async fn event_attempt_contract(&self, intent_id: &str) {
        use crate::model::{PaymentEvent, PaymentEventType};
        let mut reconciliation = PaymentEvent {
            id: format!("reconciled-1-{intent_id}"),
            intent_id: intent_id.to_owned(),
            event_type: PaymentEventType::Reconciled,
            attempt_no: 1,
            signature_ref: None,
            tx_hash: None,
            payload: serde_json::json!({"receipt": {"attempt": 1}}),
            created_at: 12,
        };
        self.ports.events.append(&reconciliation).await.unwrap();
        assert!(self
            .ports
            .events
            .find_for_intent_type(intent_id, PaymentEventType::Reconciled, 0)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            self.ports
                .events
                .find_latest_for_intent_type(intent_id, PaymentEventType::Reconciled)
                .await
                .unwrap(),
            Some(reconciliation.clone())
        );
        reconciliation.id = format!("reconciled-2-{intent_id}");
        reconciliation.attempt_no = 2;
        reconciliation.created_at = 13;
        reconciliation.payload = serde_json::json!({"receipt": {"attempt": 2}});
        self.ports.events.append(&reconciliation).await.unwrap();
        assert_eq!(
            self.ports
                .events
                .find_latest_for_intent_type(intent_id, PaymentEventType::Reconciled)
                .await
                .unwrap(),
            Some(reconciliation.clone())
        );
        let recovered_verification = PaymentEvent {
            id: format!("verified-3-{intent_id}"),
            intent_id: intent_id.to_owned(),
            event_type: PaymentEventType::CredentialVerified,
            attempt_no: 3,
            signature_ref: None,
            tx_hash: None,
            payload: serde_json::json!({"verification_ref": "recovered-verification", "recovered": true}),
            created_at: 14,
        };
        self.ports.events.append(&recovered_verification).await.unwrap();
        assert!(self
            .ports
            .events
            .find_for_intent_type(intent_id, PaymentEventType::CredentialVerified, 0)
            .await
            .unwrap()
            .is_none());
        assert_eq!(
            self.ports
                .events
                .find_latest_for_intent_type(intent_id, PaymentEventType::CredentialVerified)
                .await
                .unwrap(),
            Some(recovered_verification)
        );
        assert_eq!(
            self.ports
                .events
                .find_latest_for_intent_type(intent_id, PaymentEventType::Reconciled)
                .await
                .unwrap(),
            Some(reconciliation)
        );
    }
}
