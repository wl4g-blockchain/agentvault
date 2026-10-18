use super::*;
use crate::model::{AssetIdentity, BudgetUnit, PaymentClient, PaymentPolicy, PaymentScenario};

fn command(network: &str, recipient: &str) -> AuthorizePayment {
    AuthorizePayment {
        scenario_id: "scenario-1".to_owned(),
        run_id: "run-1".to_owned(),
        challenge: PaymentChallenge {
            scenario_id: "scenario-1".to_owned(),
            offer_id: None,
            resource: "https://merchant.example/tool".to_owned(),
            protocol: "x402".to_owned(),
            asset: AssetIdentity {
                network: network.to_owned(),
                asset: "USDC".to_owned(),
                token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
                decimals: 6,
            },
            amount: Amount::new(1),
            recipient: recipient.to_owned(),
            nonce: "nonce".to_owned(),
            challenge_hash: "challenge".to_owned(),
            expires_at: 100,
            payload: json!({}),
        },
    }
}

#[test]
fn eip155_command_requires_canonical_evm_recipient() {
    let canonical = "0xabcdef0123456789abcdef0123456789abcdef01";
    assert!(validate_command(&command("eip155:8453", canonical)).is_ok());
    for invalid in [
        "0xAbCdEf0123456789aBCDef0123456789AbCDef01",
        "external:opaque-recipient",
    ] {
        assert!(matches!(
            validate_command(&command("eip155:8453", invalid)),
            Err(Error::InvalidRequest(_))
        ));
    }
    assert!(validate_command(&command("tempo:mainnet", "external:opaque-recipient")).is_ok());
}

#[test]
fn challenge_received_event_retains_complete_public_challenge() {
    let command = command("eip155:8453", "0xabcdef0123456789abcdef0123456789abcdef01");
    let access = AccessContext {
        principal: "principal-1".to_owned(),
        client: PaymentClient {
            id: "client-1".to_owned(),
            name: "client".to_owned(),
            auth_principal: "principal-1".to_owned(),
            enabled: true,
            created_at: 1,
            updated_at: 1,
        },
        scenario: PaymentScenario {
            id: command.scenario_id.clone(),
            client_id: "client-1".to_owned(),
            external_ref: "workflow-1".to_owned(),
            name: "workflow".to_owned(),
            enabled: true,
            created_at: 1,
            updated_at: 1,
        },
    };
    let request_hash = request_hash(&command).unwrap();
    let (intent, _) = build_payment_intent(
        &command,
        "policy-1",
        "wallet-1",
        &request_hash,
        &PolicyDecision::Allowed,
        10,
    )
    .unwrap();

    let artifacts = build_authorization_artifacts(
        &access,
        &command,
        &intent,
        &PolicyDecision::Allowed,
        &request_hash,
        30,
        10,
    );
    let event = artifacts
        .events
        .iter()
        .find(|event| event.event_type == PaymentEventType::ChallengeReceived)
        .unwrap();
    let recovered: PaymentChallenge = serde_json::from_value(event.payload["challenge"].clone()).unwrap();
    assert_eq!(recovered, command.challenge);
    assert_eq!(
        event.payload["challenge_hash"].as_str(),
        Some(command.challenge.challenge_hash.as_str())
    );
}

fn candidate(wallet_id: &str, usage: u128, decision: PolicyDecision) -> PolicyWalletCandidate {
    PolicyWalletCandidate {
        policy: PaymentPolicy {
            id: "policy-1".to_owned(),
            scenario_id: "scenario-1".to_owned(),
            wallet_ids: vec![wallet_id.to_owned()],
            protocols: Vec::new(),
            networks: Vec::new(),
            assets: Vec::new(),
            budget_unit: BudgetUnit {
                asset: "USDC".to_owned(),
                decimals: 6,
            },
            allowed_recipients: Vec::new(),
            allowed_origins: Vec::new(),
            inbound_limits: None,
            outbound_limits: None,
            enabled: true,
            created_at: 1,
            updated_at: 1,
        },
        wallet_id: wallet_id.to_owned(),
        selection_key: selection_key("request", "policy-1", wallet_id),
        budget: BudgetSnapshot {
            budget_unit: BudgetUnit {
                asset: "USDC".to_owned(),
                decimals: 6,
            },
            wallet_enabled: true,
            wallet_algorithm: "secp256k1".to_owned(),
            wallet_address: "0xabcdef0123456789abcdef0123456789abcdef01".to_owned(),
            wallet_updated_at: 1,
            wallet_active_key_version: Some(1),
            wallet_shared_across_scenarios: false,
            scenario_spent_and_reserved: Amount::ZERO,
            wallet_spent_and_reserved: Amount::new(usage),
            wallet_max_per_payment: None,
            wallet_max_per_day: None,
        },
        origin: "https://merchant.example".to_owned(),
        decision,
    }
}

#[test]
fn wallet_selection_rejects_network_incompatible_local_key() {
    let mut snapshot = candidate("wallet-a", 0, PolicyDecision::Allowed).budget;
    snapshot.wallet_algorithm = "ed25519".to_owned();
    assert!(!wallet_supports_payment_network(&snapshot, "eip155:8453"));

    snapshot.wallet_algorithm = "secp256k1".to_owned();
    snapshot.wallet_address = "opaque-address".to_owned();
    assert!(!wallet_supports_payment_network(&snapshot, "eip155:8453"));
}

#[test]
fn wallet_selection_prefers_authorizable_then_least_used_candidates() {
    let denied = candidate("wallet-a", 0, PolicyDecision::Denied("disabled".to_owned()));
    let approval = candidate("wallet-b", 0, PolicyDecision::RequiresApproval);
    let busy = candidate("wallet-c", 10, PolicyDecision::Allowed);
    let idle = candidate("wallet-d", 1, PolicyDecision::Allowed);
    let mut candidates = [denied, approval, busy, idle];
    candidates.sort_by(|left, right| compare_candidates(WalletSelectionStrategy::LeastSpent, left, right));
    assert_eq!(candidates[0].wallet_id, "wallet-d");
    assert_eq!(candidates[1].wallet_id, "wallet-c");
    assert!(matches!(candidates[2].decision, PolicyDecision::RequiresApproval));
    assert!(matches!(candidates[3].decision, PolicyDecision::Denied(_)));
}

#[test]
fn wallet_selection_tie_breaker_is_stable_per_request() {
    let mut first = [
        candidate("wallet-a", 0, PolicyDecision::Allowed),
        candidate("wallet-b", 0, PolicyDecision::Allowed),
    ];
    let mut second = [
        candidate("wallet-b", 0, PolicyDecision::Allowed),
        candidate("wallet-a", 0, PolicyDecision::Allowed),
    ];
    first.sort_by(|left, right| compare_candidates(WalletSelectionStrategy::StableRandom, left, right));
    second.sort_by(|left, right| compare_candidates(WalletSelectionStrategy::StableRandom, left, right));
    assert_eq!(first[0].wallet_id, second[0].wallet_id);
}

#[test]
fn authorization_hash_excludes_internal_wallet_selection() {
    let command = command("eip155:8453", "0xabcdef0123456789abcdef0123456789abcdef01");
    let hash = request_hash(&command).unwrap();
    let (first, _) =
        build_payment_intent(&command, "policy-1", "wallet-a", &hash, &PolicyDecision::Allowed, 10).unwrap();
    let (second, _) =
        build_payment_intent(&command, "policy-2", "wallet-b", &hash, &PolicyDecision::Allowed, 10).unwrap();

    assert_eq!(first.request_hash, second.request_hash);
    assert_ne!(first.wallet_id, second.wallet_id);
    assert_ne!(first.policy_id, second.policy_id);
}
