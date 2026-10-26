use crate::storage::schema_tests::RepositoryFixture;

impl RepositoryFixture {
    pub async fn approver_contract(self) {
        let approver = crate::model::PaymentScenarioApprover {
            scenario_id: self.scenario.id.clone(),
            auth_principal: format!("admin:{}", self.client.id),
            enabled: true,
            created_at: 5,
            updated_at: 5,
        };
        self.ports.approvers.upsert(&approver).await.unwrap();
        assert!(self
            .ports
            .approvers
            .is_authorized(&self.scenario.id, &approver.auth_principal)
            .await
            .unwrap());
        self.ports
            .scenarios
            .set_enabled(&self.scenario.id, false, 6)
            .await
            .unwrap();
        assert!(!self
            .ports
            .approvers
            .is_authorized(&self.scenario.id, &approver.auth_principal)
            .await
            .unwrap());
        assert!(self
            .ports
            .approvers
            .set_enabled(&self.scenario.id, &approver.auth_principal, false, 5)
            .await
            .is_err());
    }
}
