use crate::storage::schema_tests::RepositoryFixture;

impl RepositoryFixture {
    pub async fn approval_contract(self) {
        use crate::{
            model::{Amount, ApprovalRequest, ApprovalStatus},
            storage::IdempotentWrite,
        };
        let intent = self.challenge().await;
        let approval = ApprovalRequest {
            id: format!("approval-{}", intent.id),
            intent_id: intent.id.clone(),
            run_id: intent.run_id,
            requested_amount: intent.amount,
            approved_amount: None,
            requested_by: self.client.auth_principal,
            approved_by: None,
            status: ApprovalStatus::Pending,
            intent_hash: intent.request_hash,
            grant_jti: None,
            grant_claims_hash: None,
            sealed_grant_token: None,
            expires_at: 100,
            consumed_at: None,
            created_at: 10,
            approved_at: None,
        };
        assert!(matches!(
            self.ports.approvals.request(&approval).await.unwrap(),
            IdempotentWrite::Inserted(_)
        ));
        assert_eq!(
            self.ports.approvals.request(&approval).await.unwrap(),
            IdempotentWrite::Replay(approval.clone())
        );
        let mut altered = approval.clone();
        altered.requested_amount = Amount::new(11);
        assert!(self.ports.approvals.request(&altered).await.is_err());
        assert_eq!(
            self.ports.approvals.get_active_for_intent(&intent.id).await.unwrap(),
            Some(approval)
        );
    }
}
