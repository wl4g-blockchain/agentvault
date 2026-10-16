use super::*;
use crate::model::{AssetIdentity, PaymentClient, PaymentScenario};

fn command(network: &str, recipient: &str) -> AuthorizePayment {
    AuthorizePayment {
        scenario_id: "scenario-1".to_owned(),
        run_id: "run-1".to_owned(),
        wallet_id: "wallet-1".to_owned(),
        idempotency_key: "operation-1".to_owned(),
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
    let (intent, _) = build_payment_intent(&command, "policy-1", &request_hash, &PolicyDecision::Allowed, 10).unwrap();

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
