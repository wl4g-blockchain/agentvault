use crate::storage::schema_tests::RepositoryFixture;

impl RepositoryFixture {
    pub async fn challenge(&self) -> crate::model::PaymentIntent {
        use crate::{
            model::*,
            storage::{ChallengeIssuanceGuard, ChallengeWrite, IdempotentWrite},
        };
        let id = uuid::Uuid::new_v4().to_string();
        let challenge = PaymentChallenge {
            scenario_id: self.scenario.id.clone(),
            offer_id: Some(self.offer.id.clone()),
            resource: self.offer.resource.clone(),
            protocol: "x402".into(),
            asset: self.offer.asset.clone(),
            amount: self.offer.amount,
            recipient: self.wallet.address.clone(),
            nonce: format!("nonce-{id}"),
            challenge_hash: format!("challenge-{id}"),
            expires_at: 100,
            payload: serde_json::json!({"scheme":"exact"}),
        };
        let intent = PaymentIntent {
            id: id.clone(),
            scenario_id: self.scenario.id.clone(),
            run_id: format!("run-{id}"),
            wallet_id: Some(self.wallet.id.clone()),
            policy_id: Some(self.policy.id.clone()),
            offer_id: Some(self.offer.id.clone()),
            direction: PaymentDirection::Inbound,
            protocol: "x402".into(),
            asset: self.offer.asset.clone(),
            recipient: self.wallet.address.clone(),
            amount: self.offer.amount,
            settled_amount: Amount::ZERO,
            resource: self.offer.resource.clone(),
            challenge_hash: Some(challenge.challenge_hash.clone()),
            request_hash: format!("request-{id}"),
            status: PaymentStatus::Created,
            reservation_status: ReservationStatus::Reserved,
            reserved_amount: self.offer.amount,
            tx_hash: None,
            settlement_ref: None,
            next_reconcile_at: None,
            created_at: 10,
            updated_at: 10,
            expires_at: Some(100),
            settled_at: None,
        };
        let event = PaymentEvent {
            id: format!("event-{id}"),
            intent_id: id,
            event_type: PaymentEventType::ChallengeCreated,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload: serde_json::json!({"challenge":challenge}),
            created_at: 10,
        };
        let guard = ChallengeIssuanceGuard {
            principal: self.client.auth_principal.clone(),
            client: self.client.clone(),
            scenario: self.scenario.clone(),
            policy: self.policy.clone(),
            offer: self.offer.clone(),
            wallet: self
                .ports
                .wallets
                .get_payment_wallet(&self.wallet.id)
                .await
                .unwrap()
                .unwrap(),
            origin: "https://feed.example".into(),
            day_started_at: 0,
            evaluated_at: 10,
        };
        assert!(matches!(
            self.ports
                .payments
                .record_challenge(ChallengeWrite {
                    intent: &intent,
                    challenge: &challenge,
                    guard: &guard,
                    events: &[event],
                    outbox: &[]
                })
                .await
                .unwrap(),
            IdempotentWrite::Inserted(_)
        ));
        intent
    }
}
