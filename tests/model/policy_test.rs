use super::*;

fn policy() -> PaymentPolicy {
    PaymentPolicy {
        id: "policy-1".to_owned(),
        scenario_id: "scenario-1".to_owned(),
        wallet_ids: vec!["wallet-1".to_owned(), "wallet-2".to_owned()],
        protocols: vec!["x402".to_owned()],
        networks: vec!["eip155:8453".to_owned()],
        assets: vec![AssetIdentity {
            network: "eip155:8453".to_owned(),
            asset: "USDC".to_owned(),
            token_contract: Some("0x1234".to_owned()),
            decimals: 6,
        }],
        budget_unit: BudgetUnit {
            asset: "USDC".to_owned(),
            decimals: 6,
        },
        allowed_recipients: vec!["0xabcd".to_owned()],
        allowed_origins: vec!["https://merchant.example".to_owned()],
        inbound_limits: Some(PaymentPolicyLimits {
            max_per_payment: Amount::new(20_000_000),
            max_per_day: Amount::new(200_000_000),
            approval_threshold: None,
            approval_enabled: true,
            approval_overage: Some(Amount::new(50_000_000)),
        }),
        outbound_limits: Some(PaymentPolicyLimits {
            max_per_payment: Amount::new(10_000_000),
            max_per_day: Amount::new(100_000_000),
            approval_threshold: Some(Amount::new(5_000_000)),
            approval_enabled: true,
            approval_overage: Some(Amount::new(50_000_000)),
        }),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    }
}

#[test]
fn asset_authorization_matches_complete_identity() {
    let policy = policy();
    assert!(policy.allows_asset(&policy.assets[0]));

    let mut wrong_contract = policy.assets[0].clone();
    wrong_contract.token_contract = Some("0x5678".to_owned());
    assert!(!policy.allows_asset(&wrong_contract));
}

#[test]
fn recipient_authorization_canonicalizes_only_evm_networks() {
    let mut policy = policy();
    policy.allowed_recipients = vec!["0xabcdef0123456789abcdef0123456789abcdef01".to_owned()];
    let mixed = "0xAbCdEf0123456789aBCDef0123456789AbCDef01";

    assert!(policy.allows_payment_recipient("eip155:8453", mixed));
    assert!(!policy.allows_payment_recipient("tempo:mainnet", mixed));
}

#[test]
fn one_budget_unit_can_cover_network_and_contract_variants() {
    let mut policy = policy();
    policy.networks.push("eip155:1".to_owned());
    policy.assets.push(AssetIdentity {
        network: "eip155:1".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: Some("0x5678".to_owned()),
        decimals: 6,
    });
    assert!(policy.validate().is_ok());
}

#[test]
fn mixed_asset_or_decimal_budget_units_fail_closed() {
    for (asset, decimals) in [("USDT", 6), ("USDC", 18)] {
        let mut policy = policy();
        policy.assets.push(AssetIdentity {
            network: "eip155:1".to_owned(),
            asset: asset.to_owned(),
            token_contract: Some("0x5678".to_owned()),
            decimals,
        });
        assert_eq!(policy.validate(), Err("assets"));
    }
}

#[test]
fn policy_supports_many_wallets_and_independent_direction_limits() {
    let policy = policy();
    assert!(policy.allows_wallet("wallet-1"));
    assert!(policy.allows_wallet("wallet-2"));
    assert_eq!(
        policy
            .limits_for(PaymentDirection::Inbound)
            .expect("inbound limits")
            .max_per_day,
        Amount::new(200_000_000)
    );
    assert!(policy.requires_approval(PaymentDirection::Outbound, Amount::new(5_000_000)));
    assert!(!policy.requires_approval(PaymentDirection::Inbound, Amount::new(5_000_000)));
}

#[test]
fn duplicate_or_empty_wallet_membership_fails_closed() {
    let mut policy = policy();
    policy.wallet_ids.push("wallet-1".to_owned());
    assert_eq!(policy.validate(), Err("wallet_ids"));
    policy.wallet_ids = Vec::new();
    assert_eq!(policy.validate(), Err("wallet_ids"));

    policy.wallet_ids = vec!["wallet-2".to_owned(), "wallet-1".to_owned()];
    assert_eq!(policy.validate(), Err("wallet_ids"));
}

#[test]
fn approval_overage_is_bounded_and_requires_approval_to_be_enabled() {
    let mut policy = policy();
    let limits = policy.outbound_limits.as_mut().unwrap();
    limits.approval_enabled = false;
    assert_eq!(policy.validate(), Err("outbound_limits"));

    let limits = policy.outbound_limits.as_mut().unwrap();
    limits.approval_enabled = true;
    limits.max_per_day = Amount::new(u128::MAX);
    limits.approval_overage = Some(Amount::new(1));
    assert_eq!(policy.validate(), Err("outbound_limits"));
}
