use super::*;

fn outbound_intent() -> PaymentIntent {
    PaymentIntent {
        id: "intent-1".to_owned(),
        scenario_id: "scenario-1".to_owned(),
        run_id: "run-1".to_owned(),
        wallet_id: Some("wallet-1".to_owned()),
        policy_id: Some("policy-1".to_owned()),
        offer_id: None,
        direction: PaymentDirection::Outbound,
        protocol: "x402".to_owned(),
        asset: AssetIdentity {
            network: "eip155:8453".to_owned(),
            asset: "USDC".to_owned(),
            token_contract: Some("0x1234".to_owned()),
            decimals: 6,
        },
        recipient: "0xabcd".to_owned(),
        amount: Amount::new(1_000_000),
        settled_amount: Amount::ZERO,
        resource: "https://merchant.example/tool".to_owned(),
        challenge_hash: Some("challenge-hash".to_owned()),
        request_hash: "request-hash".to_owned(),
        status: PaymentStatus::Created,
        reservation_status: ReservationStatus::Unreserved,
        reserved_amount: Amount::ZERO,
        tx_hash: None,
        settlement_ref: None,
        next_reconcile_at: None,
        created_at: 1,
        updated_at: 1,
        expires_at: Some(60),
        settled_at: None,
    }
}

#[test]
fn amount_json_is_precision_safe() {
    let amount = Amount::new(u128::MAX);
    let encoded = serde_json::to_string(&amount).unwrap();
    assert_eq!(encoded, format!("\"{}\"", u128::MAX));
    assert_eq!(serde_json::from_str::<Amount>(&encoded).unwrap(), amount);
    assert!(serde_json::from_str::<Amount>("42").is_err());
}

#[test]
fn budget_unit_excludes_network_and_token_contract() {
    let first = outbound_intent().asset;
    let mut second = first.clone();
    second.network = "eip155:1".to_owned();
    second.token_contract = Some("0x5678".to_owned());
    assert_eq!(first.budget_unit(), second.budget_unit());

    second.decimals = 18;
    assert_ne!(first.budget_unit(), second.budget_unit());
}

#[test]
fn run_replay_distinguishes_same_and_conflicting_request() {
    let intent = outbound_intent();

    assert_eq!(intent.check_replay("request-hash"), ReplayCheck::Replay);
    assert_eq!(intent.check_replay("other-hash"), ReplayCheck::Conflict);
}

#[test]
fn state_transitions_cannot_rewrite_payment_identity() {
    let original = outbound_intent();
    let mut transitioned = original.clone();
    transitioned.status = PaymentStatus::Authorized;
    transitioned.reservation_status = ReservationStatus::Reserved;
    transitioned.reserved_amount = transitioned.amount;
    transitioned.updated_at += 1;
    assert!(original.same_immutable_identity_as(&transitioned));

    transitioned.recipient = "0xother".to_owned();
    assert!(!original.same_immutable_identity_as(&transitioned));
}

#[test]
fn direction_requires_the_authoritative_binding() {
    let mut outbound = outbound_intent();
    assert_eq!(outbound.authority_ids(), Ok(("policy-1", "wallet-1")));
    outbound.policy_id = None;
    assert_eq!(outbound.validate(), Err("policy_id"));
    assert_eq!(outbound.authority_ids(), Err("policy_id"));

    let mut inbound = outbound_intent();
    inbound.direction = PaymentDirection::Inbound;
    assert_eq!(inbound.validate(), Err("offer_id"));
    inbound.offer_id = Some("offer-1".to_owned());
    assert!(inbound.validate().is_ok());

    inbound.policy_id = None;
    assert_eq!(inbound.validate(), Err("policy_id"));

    let mut outbound_with_offer = outbound_intent();
    outbound_with_offer.offer_id = Some("offer-1".to_owned());
    assert_eq!(outbound_with_offer.validate(), Err("offer_id"));
}
