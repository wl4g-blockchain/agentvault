use super::*;

fn outbound_intent() -> PaymentIntent {
    PaymentIntent {
        id: "intent-1".to_owned(),
        scenario_id: "scenario-1".to_owned(),
        run_id: "run-1".to_owned(),
        wallet_id: Some("wallet-1".to_owned()),
        policy_id: Some("policy-1".to_owned()),
        price_id: None,
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
