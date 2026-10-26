use crate::storage::schema_tests::RepositoryFixture;

impl RepositoryFixture {
    pub async fn policy_contract(self) {
        assert_eq!(
            self.ports.policies.get(&self.policy.id).await.unwrap(),
            Some(self.policy.clone())
        );
        let mut revised = self.policy.clone();
        revised.updated_at += 1;
        revised.enabled = false;
        assert!(self.ports.policies.update(&revised, 0).await.is_err());
        assert_eq!(
            self.ports.policies.get(&self.policy.id).await.unwrap(),
            Some(self.policy.clone())
        );
        assert_eq!(
            self.ports
                .policies
                .update(&revised, self.policy.updated_at)
                .await
                .unwrap(),
            revised
        );
        let binding = self
            .ports
            .wallets
            .get_payment_wallet(&self.wallet.id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(binding.budget_unit, Some(self.policy.budget_unit));
    }
}
