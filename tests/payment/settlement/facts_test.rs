use super::*;
use crate::model::AssetIdentity;
#[test]
fn evidence_hash_is_stable_and_domain_separated() {
    let evidence = json!({"tx": "0xabc", "amount": "100"});
    assert_eq!(
        SettlementFacts::evidence_hash(&evidence).unwrap(),
        SettlementFacts::evidence_hash(&evidence).unwrap()
    );
    assert_ne!(
        SettlementFacts::evidence_hash(&evidence).unwrap(),
        SettlementFacts::evidence_hash(&json!({"tx": "0xdef"})).unwrap()
    );
}

#[test]
fn authorization_expiry_requires_both_wall_clock_and_scanned_chain_time() {
    assert!(!SettlementFacts::authorization_is_definitively_expired(100, 99, 100));
    assert!(!SettlementFacts::authorization_is_definitively_expired(99, 100, 100));
    assert!(SettlementFacts::authorization_is_definitively_expired(100, 100, 100));
}

#[test]
fn terminal_non_settlement_releases_only_an_outstanding_reservation() {
    assert_eq!(
        SettlementFacts::reservation_after_non_settlement(PaymentStatus::Expired, ReservationStatus::Reserved),
        ReservationStatus::Released
    );
    assert_eq!(
        SettlementFacts::reservation_after_non_settlement(PaymentStatus::Failed, ReservationStatus::Reserved),
        ReservationStatus::Released
    );
    assert_eq!(
        SettlementFacts::reservation_after_non_settlement(PaymentStatus::Unknown, ReservationStatus::Reserved),
        ReservationStatus::Reserved
    );
    assert_eq!(
        SettlementFacts::reservation_after_non_settlement(PaymentStatus::Expired, ReservationStatus::Consumed),
        ReservationStatus::Consumed
    );
}

#[test]
fn durable_receipt_falls_back_to_reconciliation_event() {
    let receipt = PaymentReceipt {
        intent_id: "intent-1".to_owned(),
        scenario_id: "scenario-1".to_owned(),
        direction: PaymentDirection::Inbound,
        protocol: "x402".to_owned(),
        asset: AssetIdentity {
            network: "eip155:31337".to_owned(),
            asset: "USDC".to_owned(),
            token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
            decimals: 6,
        },
        recipient: "0x2222222222222222222222222222222222222222".to_owned(),
        payer: Some("0x3333333333333333333333333333333333333333".to_owned()),
        requested_amount: Amount::new(10),
        settled_amount: Amount::new(10),
        status: PaymentStatus::Settled,
        tx_hash: Some(format!("0x{}", "ab".repeat(32))),
        settlement_ref: None,
        evidence_hash: "evidence".to_owned(),
        confirmed_at: 10,
    };
    let reconciled = PaymentEvent {
        id: "event-1".to_owned(),
        intent_id: receipt.intent_id.clone(),
        event_type: PaymentEventType::Reconciled,
        attempt_no: 1,
        signature_ref: None,
        tx_hash: receipt.tx_hash.clone(),
        payload: json!({"receipt": receipt}),
        created_at: 10,
    };

    let recovered = SettlementFacts::durable_receipt(None, Some(&reconciled)).unwrap();
    assert_eq!(recovered.intent_id, "intent-1");
    assert_eq!(recovered.status, PaymentStatus::Settled);
}

#[test]
fn concurrent_settlement_replay_requires_same_terminal_payment_identity() {
    let expected = pending_inbound_intent("x402");
    let mut winner = expected.clone();
    winner.status = PaymentStatus::Settled;
    winner.updated_at += 1;
    winner.settled_at = Some(winner.updated_at);

    assert!(SettlementFacts::is_concurrent_settlement_winner(&expected, &winner));

    let mut foreign = winner.clone();
    foreign.wallet_id = Some("wallet-other".to_owned());
    assert!(!SettlementFacts::is_concurrent_settlement_winner(&expected, &foreign));

    winner.status = PaymentStatus::Unknown;
    winner.settled_at = None;
    assert!(!SettlementFacts::is_concurrent_settlement_winner(&expected, &winner));
}

#[test]
fn mpp_confirmed_evidence_builds_matching_payment_receipt_and_ledger() {
    let intent = pending_inbound_intent("mpp");
    let transaction = format!("0x{}", "ab".repeat(32));
    let result = SettlementProviderResult {
        valid: true,
        settled: true,
        payer: Some("0x3333333333333333333333333333333333333333".to_owned()),
        transaction: Some(transaction.clone()),
        settlement_ref: None,
        effect_ref: Some("tempo:tip20-transfer:0".to_owned()),
        invalid_reason: None,
        asset: Some(intent.asset.clone()),
        amount: Some(intent.amount),
        recipient: Some(intent.recipient.clone()),
        confirmations: Some(1),
        extensions: BTreeMap::from([
            ("gas_fee".to_owned(), json!("474")),
            ("gas_used".to_owned(), json!(295_999)),
        ]),
    };
    result.validate_confirmed_for(&intent, 1).unwrap();

    let settled = SettlementFacts::confirmed_intent(&intent, &result, 101, 100);
    let receipt = SettlementFacts::confirmed_receipt(&intent, &result, 100).unwrap();
    let ledger = SettlementFacts::confirmed_ledger(&intent, "tempo-localnet:1337", &result, 100, 101).unwrap();
    let event = SettlementFacts::confirmed_event(&intent, &result, &receipt, 101);

    assert_eq!(settled.status, PaymentStatus::Settled);
    assert_eq!(settled.reservation_status, ReservationStatus::Consumed);
    assert_eq!(settled.tx_hash.as_deref(), Some(transaction.as_str()));
    assert_eq!(receipt.protocol, "mpp");
    assert_eq!(receipt.tx_hash.as_deref(), Some(transaction.as_str()));
    assert_eq!(ledger.entry_type, LedgerEntryType::Payment);
    assert_eq!(ledger.direction, PaymentDirection::Inbound);
    assert_eq!(ledger.amount, intent.amount);
    assert_eq!(ledger.tx_hash.as_deref(), Some(transaction.as_str()));
    assert_eq!(ledger.effect_ref.as_deref(), Some("tempo:tip20-transfer:0"));
    assert_eq!(event.event_type, PaymentEventType::Settled);
    assert_eq!(event.payload["receipt"]["protocol"], "mpp");
}

#[test]
fn confirmed_intent_clamps_bounded_provider_clock_skew_to_creation_time() {
    let intent = pending_inbound_intent("mpp");
    let result = SettlementProviderResult {
        valid: true,
        settled: true,
        transaction: Some(format!("0x{}", "ab".repeat(32))),
        settlement_ref: None,
        payer: Some("0x1111111111111111111111111111111111111111".to_owned()),
        recipient: Some(intent.recipient.clone()),
        asset: Some(intent.asset.clone()),
        amount: Some(intent.amount),
        effect_ref: Some("log:0".to_owned()),
        confirmations: Some(1),
        invalid_reason: None,
        extensions: BTreeMap::new(),
    };

    let settled = SettlementFacts::confirmed_intent(&intent, &result, 101, intent.created_at - 1);

    assert_eq!(settled.settled_at, Some(intent.created_at));
}

pub(crate) fn pending_inbound_intent(protocol: &str) -> PaymentIntent {
    let (network, asset, token_contract) = if protocol == "mpp" {
        ("eip155:1337", "pathUSD", "0x20c0000000000000000000000000000000000000")
    } else {
        ("eip155:31337", "USDC", "0x1111111111111111111111111111111111111111")
    };
    PaymentIntent {
        id: "intent-1".to_owned(),
        scenario_id: "scenario-1".to_owned(),
        run_id: "run-1".to_owned(),
        wallet_id: Some("wallet-1".to_owned()),
        policy_id: Some("policy-1".to_owned()),
        price_id: Some("price-1".to_owned()),
        direction: PaymentDirection::Inbound,
        protocol: protocol.to_owned(),
        asset: AssetIdentity {
            network: network.to_owned(),
            asset: asset.to_owned(),
            token_contract: Some(token_contract.to_owned()),
            decimals: 6,
        },
        recipient: "0x2222222222222222222222222222222222222222".to_owned(),
        amount: Amount::new(1_000_000),
        settled_amount: Amount::ZERO,
        resource: "https://merchant.example/mcp".to_owned(),
        challenge_hash: Some("challenge".to_owned()),
        request_hash: "request".to_owned(),
        status: PaymentStatus::PendingSettlement,
        reservation_status: ReservationStatus::Reserved,
        reserved_amount: Amount::new(1_000_000),
        tx_hash: None,
        settlement_ref: None,
        next_reconcile_at: Some(100),
        created_at: 90,
        updated_at: 95,
        expires_at: Some(1_000),
        settled_at: None,
    }
}
