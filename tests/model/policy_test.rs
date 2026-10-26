use serde_json::json;

use super::*;
use crate::model::AssetIdentity;

fn fixture() -> (PaymentPolicy, PaymentChallenge) {
    let asset = AssetIdentity {
        network: "eip155:8453".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
        decimals: 6,
    };
    (
        PaymentPolicy {
            id: "policy".to_owned(),
            scenario_id: "scenario".to_owned(),
            wallet_ids: vec!["wallet".to_owned()],
            protocols: vec!["x402".to_owned()],
            networks: vec![asset.network.clone()],
            assets: vec![asset.clone()],
            budget_unit: asset.budget_unit(),
            allowed_recipients: vec!["0x2222222222222222222222222222222222222222".to_owned()],
            allowed_origins: vec!["https://merchant.example".to_owned()],
            inbound_limits: Some(crate::model::PaymentPolicyLimits {
                max_per_payment: Amount::new(100),
                max_per_day: Amount::new(150),
                approval_threshold: None,
                approval_enabled: true,
                approval_overage: Some(Amount::new(50)),
            }),
            outbound_limits: Some(crate::model::PaymentPolicyLimits {
                max_per_payment: Amount::new(100),
                max_per_day: Amount::new(150),
                approval_threshold: Some(Amount::new(80)),
                approval_enabled: true,
                approval_overage: Some(Amount::new(50)),
            }),
            enabled: true,
            created_at: 1,
            updated_at: 1,
        },
        PaymentChallenge {
            scenario_id: "scenario".to_owned(),
            price_id: None,
            resource: "https://merchant.example/tool".to_owned(),
            protocol: "x402".to_owned(),
            asset,
            amount: Amount::new(80),
            recipient: "0x2222222222222222222222222222222222222222".to_owned(),
            nonce: "nonce".to_owned(),
            challenge_hash: "hash".to_owned(),
            expires_at: 100,
            payload: json!({}),
        },
    )
}

#[test]
fn enforces_wallet_aggregate_before_approval() {
    let (policy, challenge) = fixture();
    let decision = PolicyEngine::evaluate(
        &policy,
        PolicyEvaluation {
            direction: PaymentDirection::Outbound,
            challenge: &challenge,
            wallet_id: "wallet",
            origin: "https://merchant.example",
            scenario_usage: Amount::ZERO,
            wallet_budget: WalletBudget {
                usage: Amount::new(30),
                shared_across_scenarios: true,
                max_per_payment: Some(Amount::new(100)),
                max_per_day: Some(Amount::new(100)),
            },
            now: 1,
        },
    )
    .unwrap();
    assert!(matches!(decision, PolicyDecision::Denied(reason) if reason.contains("shared-wallet")));
}

#[test]
fn approval_does_not_bypass_normal_limits() {
    let (policy, challenge) = fixture();
    assert_eq!(
        PolicyEngine::evaluate(
            &policy,
            PolicyEvaluation {
                direction: PaymentDirection::Outbound,
                challenge: &challenge,
                wallet_id: "wallet",
                origin: "https://merchant.example",
                scenario_usage: Amount::ZERO,
                wallet_budget: WalletBudget {
                    usage: Amount::ZERO,
                    shared_across_scenarios: false,
                    max_per_payment: None,
                    max_per_day: None,
                },
                now: 1,
            },
        )
        .unwrap(),
        PolicyDecision::RequiresApproval
    );
}

#[test]
fn shared_wallet_without_hard_limits_fails_closed() {
    let (policy, challenge) = fixture();
    let decision = PolicyEngine::evaluate(
        &policy,
        PolicyEvaluation {
            direction: PaymentDirection::Outbound,
            challenge: &challenge,
            wallet_id: "wallet",
            origin: "https://merchant.example",
            scenario_usage: Amount::ZERO,
            wallet_budget: WalletBudget {
                usage: Amount::ZERO,
                shared_across_scenarios: true,
                max_per_payment: None,
                max_per_day: None,
            },
            now: 1,
        },
    )
    .unwrap();
    assert!(matches!(decision, PolicyDecision::Denied(reason) if reason.contains("shared-wallet")));
}

#[test]
fn inbound_and_outbound_have_independent_daily_limits() {
    let (mut policy, challenge) = fixture();
    policy.inbound_limits.as_mut().unwrap().max_per_payment = Amount::new(90);
    policy.inbound_limits.as_mut().unwrap().max_per_day = Amount::new(90);
    policy.inbound_limits.as_mut().unwrap().approval_overage = None;
    policy.inbound_limits.as_mut().unwrap().approval_enabled = false;

    let inbound = PolicyEngine::evaluate(
        &policy,
        PolicyEvaluation {
            direction: PaymentDirection::Inbound,
            challenge: &challenge,
            wallet_id: "wallet",
            origin: "https://merchant.example",
            scenario_usage: Amount::new(20),
            wallet_budget: WalletBudget::default(),
            now: 1,
        },
    )
    .unwrap();
    assert!(matches!(inbound, PolicyDecision::Denied(reason) if reason.contains("daily")));

    let outbound = PolicyEngine::evaluate(
        &policy,
        PolicyEvaluation {
            direction: PaymentDirection::Outbound,
            challenge: &challenge,
            wallet_id: "wallet",
            origin: "https://merchant.example",
            scenario_usage: Amount::new(20),
            wallet_budget: WalletBudget::default(),
            now: 1,
        },
    )
    .unwrap();
    assert_eq!(outbound, PolicyDecision::RequiresApproval);
}

#[test]
fn approval_overage_never_bypasses_hard_ceiling() {
    let (policy, challenge) = fixture();
    let decision = PolicyEngine::evaluate(
        &policy,
        PolicyEvaluation {
            direction: PaymentDirection::Outbound,
            challenge: &challenge,
            wallet_id: "wallet",
            origin: "https://merchant.example",
            scenario_usage: Amount::new(121),
            wallet_budget: WalletBudget::default(),
            now: 1,
        },
    )
    .unwrap();
    assert!(matches!(decision, PolicyDecision::Denied(reason) if reason.contains("hard ceiling")));
}

#[test]
fn soft_daily_limit_uses_one_time_approval_headroom() {
    let (mut policy, challenge) = fixture();
    policy.outbound_limits.as_mut().unwrap().approval_threshold = None;
    let decision = PolicyEngine::evaluate(
        &policy,
        PolicyEvaluation {
            direction: PaymentDirection::Outbound,
            challenge: &challenge,
            wallet_id: "wallet",
            origin: "https://merchant.example",
            scenario_usage: Amount::new(80),
            wallet_budget: WalletBudget::default(),
            now: 1,
        },
    )
    .unwrap();
    assert_eq!(decision, PolicyDecision::RequiresApproval);
}
