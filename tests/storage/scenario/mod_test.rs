use crate::storage::schema_tests::RepositoryFixture;

impl RepositoryFixture {
    pub async fn scenario_contract(self) {
        let mut scenario = self.scenario.clone();
        scenario.id.push_str("-atomic");
        let mut policy = self.policy.clone();
        policy.id.push_str("-atomic");
        policy.scenario_id = scenario.id.clone();
        policy.created_at = scenario.created_at;
        self.ports
            .scenarios
            .register_with_policy(&scenario, &policy)
            .await
            .unwrap();
        let mut replay = scenario.clone();
        replay.created_at += 1;
        replay.updated_at += 1;
        let mut replay_policy = policy.clone();
        replay_policy.created_at = replay.created_at;
        replay_policy.updated_at += 1;
        let result = self
            .ports
            .scenarios
            .register_with_policy(&replay, &replay_policy)
            .await
            .unwrap();
        match result {
            crate::storage::IdempotentWrite::Replay(configuration) => assert_eq!(configuration.scenario, scenario),
            crate::storage::IdempotentWrite::Inserted(_) => panic!("scenario retry inserted a second aggregate"),
        }
        let mut invalid = scenario.clone();
        invalid.id.push_str("-invalid");
        policy.id.push_str("-invalid");
        policy.scenario_id = invalid.id.clone();
        policy.wallet_ids = vec!["missing-wallet".into()];
        assert!(self
            .ports
            .scenarios
            .register_with_policy(&invalid, &policy)
            .await
            .is_err());
        assert!(self.ports.scenarios.get(&invalid.id).await.unwrap().is_none());
    }
}
