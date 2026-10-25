//! `PaymentLedgerEntry` persistence-entity invariants.

use super::*;

fn entry(entry_type: LedgerEntryType) -> PaymentLedgerEntry {
    PaymentLedgerEntry {
        id: "ledger-1".to_owned(),
        intent_id: Some("intent-1".to_owned()),
        scenario_id: Some("scenario-1".to_owned()),
        wallet_id: "wallet-1".to_owned(),
        entry_type,
        direction: PaymentDirection::Outbound,
        asset: AssetIdentity {
            network: "eip155:8453".to_owned(),
            asset: "USDC".to_owned(),
            token_contract: Some("0x1234".to_owned()),
            decimals: 6,
        },
        amount: Amount::new(100),
        posting_key: "provider:payment-1".to_owned(),
        tx_hash: Some("0xtransaction".to_owned()),
        settlement_ref: None,
        effect_ref: None,
        transfer_ref: None,
        reversal_of: None,
        effective_at: 2,
        created_at: 3,
    }
}

#[test]
fn reversal_must_reference_original_entry() {
    assert_eq!(entry(LedgerEntryType::Reversal).validate(), Err("reversal_of"));

    let mut reversal = entry(LedgerEntryType::Reversal);
    reversal.reversal_of = Some("ledger-original".to_owned());
    assert!(reversal.validate().is_ok());
}

#[test]
fn reversal_is_an_exact_opposite_wallet_fact() {
    let original = entry(LedgerEntryType::Payment);
    let mut reversal = original.clone();
    reversal.id = "ledger-reversal".to_owned();
    reversal.entry_type = LedgerEntryType::Reversal;
    reversal.direction = PaymentDirection::Inbound;
    reversal.posting_key = "provider:reversal-1".to_owned();
    reversal.reversal_of = Some(original.id.clone());
    assert!(reversal.validate_reversal_of(&original).is_ok());

    let mut wrong_wallet = reversal.clone();
    wrong_wallet.wallet_id = "wallet-2".to_owned();
    assert_eq!(wrong_wallet.validate_reversal_of(&original), Err("reversal_binding"));

    let mut wrong_asset = reversal.clone();
    wrong_asset.asset.asset = "USDT".to_owned();
    assert_eq!(wrong_asset.validate_reversal_of(&original), Err("reversal_binding"));

    let mut wrong_amount = reversal.clone();
    wrong_amount.amount = Amount::new(99);
    assert_eq!(wrong_amount.validate_reversal_of(&original), Err("reversal_binding"));

    let mut same_direction = reversal.clone();
    same_direction.direction = original.direction;
    assert_eq!(same_direction.validate_reversal_of(&original), Err("reversal_binding"));

    let mut reversal_of_reversal = reversal.clone();
    reversal_of_reversal.reversal_of = Some(reversal.id.clone());
    assert_eq!(
        reversal_of_reversal.validate_reversal_of(&reversal),
        Err("reversal_binding")
    );
}

#[test]
fn posting_key_identifies_evidence_not_business_interpretation() {
    let first = settlement_posting_key("facilitator-a", "eip155:8453", None, Some("payment-1"), None).unwrap();
    let second = settlement_posting_key("facilitator-a", "eip155:8453", None, Some("payment-1"), None).unwrap();
    assert_eq!(first, second);

    let upper = settlement_posting_key(
        "rpc-a",
        "eip155:8453",
        Some("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
        None,
        None,
    )
    .unwrap();
    let lower = settlement_posting_key(
        "rpc-b",
        "eip155:8453",
        Some("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        None,
        None,
    )
    .unwrap();
    assert_eq!(upper, lower);

    let same_tx_different_provider_ref = settlement_posting_key(
        "facilitator-a",
        "eip155:8453",
        Some("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        Some("different-provider-reference"),
        None,
    )
    .unwrap();
    assert_eq!(lower, same_tx_different_provider_ref);

    let token_log = settlement_posting_key(
        "rpc-a",
        "eip155:8453",
        Some("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        None,
        Some("log:7"),
    )
    .unwrap();
    let gas = settlement_posting_key(
        "rpc-a",
        "eip155:8453",
        Some("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
        None,
        Some("gas"),
    )
    .unwrap();
    assert_ne!(token_log, gas);
    assert_ne!(token_log, lower);
    assert_eq!(
        settlement_posting_key(
            "rpc-b",
            "eip155:8453",
            Some("0xAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"),
            Some("ignored-when-transaction-is-present"),
            Some(" log:7 "),
        )
        .unwrap(),
        token_log
    );
    assert_eq!(
        settlement_posting_key(
            "rpc-a",
            "eip155:8453",
            Some("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"),
            None,
            Some("  ")
        ),
        Err("effect_ref")
    );

    let original = entry(LedgerEntryType::Payment);
    let mut conflicting = original.clone();
    conflicting.amount = Amount::new(101);
    assert!(original.same_posting_as(&conflicting));
    assert!(!original.same_fact_content_as(&conflicting));
}

#[test]
fn posting_key_accepts_only_canonical_supported_chain_transactions() {
    let solana_signature = "3gPVDLMYKWaWcTcwd7rfvGDCL3PUVzDf5CgFTFtT7uiRkefs3m4sQhW6jPfnRd4pwtUBcR2M5yXAyQwbAouh9kpU";
    let solana = settlement_posting_key(
        "solana-rpc:local",
        "solana:4sGjMW1sUnHzSxGspuhpqLDx6wiyjNtZ",
        Some(solana_signature),
        None,
        Some("spl-transfer"),
    )
    .unwrap();
    assert!(solana.starts_with("chain-effect:"));
    assert_eq!(
        settlement_posting_key(
            "solana-rpc:local",
            "solana:4sGjMW1sUnHzSxGspuhpqLDx6wiyjNtZ",
            Some("not-a-signature"),
            None,
            None,
        ),
        Err("tx_hash")
    );
    assert_eq!(
        settlement_posting_key("rpc", "cosmos:local", Some(solana_signature), None, None),
        Err("network")
    );
}
