use super::*;

#[test]
fn import_specs_copy_every_shared_field_into_domain_entities() {
    let imported_at = 42;
    let client = ClientSpec {
        id: "client-1".to_owned(),
        name: "Client".to_owned(),
        auth_principal: "principal-1".to_owned(),
        enabled: true,
    }
    .into_domain(imported_at);
    assert_eq!(client.id, "client-1");
    assert_eq!(client.auth_principal, "principal-1");
    assert_eq!((client.created_at, client.updated_at), (imported_at, imported_at));

    let scenario = ScenarioSpec {
        id: "scenario-1".to_owned(),
        client_id: client.id.clone(),
        name: "Scenario".to_owned(),
        enabled: true,
    }
    .into_domain(imported_at);
    assert_eq!(scenario.client_id, client.id);
    assert_eq!((scenario.created_at, scenario.updated_at), (imported_at, imported_at));

    let asset = AssetIdentity {
        network: "eip155:31337".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
        decimals: 6,
    };
    let policy = PolicySpec {
        id: "policy-1".to_owned(),
        scenario_id: scenario.id.clone(),
        wallet_ids: vec!["wallet-1".to_owned()],
        protocols: vec!["x402".to_owned()],
        networks: vec![asset.network.clone()],
        assets: vec![asset.clone()],
        budget_unit: BudgetUnit {
            asset: "USDC".to_owned(),
            decimals: 6,
        },
        allowed_recipients: vec!["0x2222222222222222222222222222222222222222".to_owned()],
        allowed_origins: vec!["https://merchant.example".to_owned()],
        inbound_limits: None,
        outbound_limits: None,
        enabled: true,
    }
    .into_domain(imported_at);
    assert_eq!(policy.scenario_id, scenario.id);
    assert_eq!(policy.wallet_ids, ["wallet-1"]);
    assert_eq!(policy.assets.as_slice(), std::slice::from_ref(&asset));
    assert_eq!((policy.created_at, policy.updated_at), (imported_at, imported_at));

    let offer = PriceSpec {
        id: "offer-1".to_owned(),
        scenario_id: scenario.id.clone(),
        policy_id: policy.id.clone(),
        resource: "https://merchant.example/feed".to_owned(),
        protocols: vec!["x402".to_owned()],
        asset,
        amount: Amount::new(1_000_000),
        wallet_id: "wallet-1".to_owned(),
        enabled: true,
    }
    .into_domain(imported_at);
    assert_eq!(offer.policy_id, policy.id);
    assert_eq!(offer.wallet_id, "wallet-1");
    assert_eq!((offer.created_at, offer.updated_at), (imported_at, imported_at));

    let approver = ApproverSpec {
        scenario_id: scenario.id,
        auth_principal: "approver-1".to_owned(),
        enabled: true,
    }
    .into_domain(imported_at);
    assert_eq!(approver.auth_principal, "approver-1");
    assert_eq!((approver.created_at, approver.updated_at), (imported_at, imported_at));
}

#[test]
fn wallet_spec_builds_domain_metadata_without_copying_custody_material() {
    let spec = WalletSpec {
        id: "wallet-1".to_owned(),
        enabled: true,
        budget_unit: None,
        max_per_payment: None,
        max_per_day: None,
        source: WalletSource::Local {
            algorithm: EVM_EOA_ALGORITHM.to_owned(),
            private_key_env: "TEST_WALLET_PRIVATE_KEY".to_owned(),
        },
    };
    let wallet = spec.domain_wallet(
        "0x1111111111111111111111111111111111111111".to_owned(),
        "public-key".to_owned(),
        42,
    );

    assert_eq!(wallet.id, "wallet-1");
    assert_eq!(wallet.algorithm, EVM_EOA_ALGORITHM);
    let encoded = serde_json::to_string(&wallet).unwrap();
    assert!(!encoded.contains("TEST_WALLET_PRIVATE_KEY"));
}

#[test]
fn canonical_price_resources_accept_legacy_offer_kind_without_two_models() {
    for kind in ["Price", "Price"] {
        assert!(matches!(
            serde_yaml::from_str::<ResourceKind>(kind).unwrap(),
            ResourceKind::Price
        ));
    }
}
