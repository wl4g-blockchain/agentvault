use crate::model::Amount;
use crate::model::AssetIdentity;
use crate::model::PaymentClient;
use crate::model::PaymentPolicy;
use crate::model::PaymentPolicyLimits;
use crate::model::PaymentScenario;
use crate::storage::client::sqlite::ClientSqliteRepository;
use crate::storage::policy::sqlite::PolicySqliteRepository;
use crate::storage::scenario::sqlite::ScenarioSqliteRepository;
use crate::storage::sqlite::tests::*;
use crate::storage::wallet::sqlite::SqliteWalletKeyRepository;
use crate::storage::IdempotentWrite;
use std::sync::Arc;

#[tokio::test]
async fn registers_scenario_and_multi_wallet_policy_atomically() {
    let (_directory, storage) = storage().await;
    let repository =
        SqliteWalletKeyRepository::from_resolved_keys(storage.clone(), "kek-v1", [("kek-v1".to_owned(), [3_u8; 32])])
            .unwrap();
    let wallet_a = repository
        .create_local("wallet-aggregate-a", &[4_u8; 32], 1)
        .await
        .unwrap();
    let wallet_b = repository
        .create_local("wallet-aggregate-b", &[5_u8; 32], 1)
        .await
        .unwrap();
    let client = PaymentClient {
        id: "client-aggregate".to_owned(),
        name: "Aggregate client".to_owned(),
        auth_principal: "principal:aggregate".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    crate::storage::IClientRepository::insert(&ClientSqliteRepository::new(storage.clone()), &client)
        .await
        .unwrap();
    let scenario = PaymentScenario {
        id: "scenario-aggregate".to_owned(),
        client_id: client.id.clone(),
        name: "Aggregate scenario".to_owned(),
        enabled: true,
        created_at: 2,
        updated_at: 2,
    };
    let asset = AssetIdentity {
        network: "eip155:31337".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
        decimals: 6,
    };
    let policy = PaymentPolicy {
        id: "policy-aggregate".to_owned(),
        scenario_id: scenario.id.clone(),
        wallet_ids: vec![wallet_a.id.clone(), wallet_b.id.clone()],
        protocols: vec!["x402".to_owned()],
        networks: vec![asset.network.clone()],
        assets: vec![asset.clone()],
        budget_unit: asset.budget_unit(),
        allowed_recipients: vec!["0x2222222222222222222222222222222222222222".to_owned()],
        allowed_origins: vec!["https://merchant.example".to_owned()],
        inbound_limits: None,
        outbound_limits: Some(PaymentPolicyLimits {
            max_per_payment: Amount::new(10),
            max_per_day: Amount::new(100),
            approval_threshold: None,
            approval_enabled: true,
            approval_overage: None,
        }),
        enabled: true,
        created_at: 2,
        updated_at: 2,
    };
    let inserted = crate::storage::IScenarioRepository::register_with_policy(
        &ScenarioSqliteRepository::new(storage.clone()),
        &scenario,
        &policy,
    )
    .await
    .unwrap();
    assert!(matches!(inserted, IdempotentWrite::Inserted(_)));
    let mut retried_scenario = scenario.clone();
    retried_scenario.created_at = 200;
    retried_scenario.updated_at = 200;
    let mut retried_policy = policy.clone();
    retried_policy.created_at = 200;
    retried_policy.updated_at = 200;
    let replay = crate::storage::IScenarioRepository::register_with_policy(
        &ScenarioSqliteRepository::new(storage.clone()),
        &retried_scenario,
        &retried_policy,
    )
    .await
    .unwrap();
    let IdempotentWrite::Replay(replayed) = replay else {
        panic!("same business command must replay")
    };
    assert_eq!(replayed.scenario.created_at, scenario.created_at);
    assert_eq!(replayed.policy.created_at, policy.created_at);
    assert_eq!(
        crate::storage::IPolicyRepository::get(&PolicySqliteRepository::new(storage.clone()), &policy.id)
            .await
            .unwrap()
            .unwrap()
            .wallet_ids,
        policy.wallet_ids
    );

    assert_invalid_wallet_rolls_back(&storage, &scenario, &policy).await;
}

use crate::storage::schema_tests::RepositoryFixture;

#[tokio::test]
async fn preserves_scenario_contract_through_composed_repository() {
    let (_directory, storage) = storage().await;
    RepositoryFixture::create(Arc::new(storage))
        .await
        .scenario_contract()
        .await;
}
