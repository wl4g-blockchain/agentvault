use super::*;
use crate::model::AssetIdentity;
#[test]
fn advertised_expiry_accepts_only_canonical_unix_seconds() {
    assert_eq!(PaymentApi::parse_advertised_expiry(None).unwrap(), None);
    assert_eq!(
        PaymentApi::parse_advertised_expiry(Some("1791922093")).unwrap(),
        Some(1_791_922_093)
    );
    for invalid in ["", "0x12", "-1", "+1", "01", "18446744073709551615"] {
        assert!(matches!(
            PaymentApi::parse_advertised_expiry(Some(invalid)),
            Err(Error::InvalidRequest(_))
        ));
    }
}

fn entry(
    id: &str,
    entry_type: LedgerEntryType,
    direction: PaymentDirection,
    amount: u128,
    reversal_of: Option<&str>,
) -> PaymentLedgerEntry {
    PaymentLedgerEntry {
        id: id.to_owned(),
        intent_id: Some("intent-1".to_owned()),
        scenario_id: Some("scenario-1".to_owned()),
        wallet_id: "wallet-1".to_owned(),
        entry_type,
        direction,
        asset: asset(),
        amount: Amount::new(amount),
        posting_key: format!("posting-{id}"),
        tx_hash: Some(format!("0x{id}")),
        settlement_ref: None,
        effect_ref: None,
        transfer_ref: None,
        reversal_of: reversal_of.map(str::to_owned),
        effective_at: 10,
        created_at: 10,
    }
}

#[test]
fn balance_reversals_negate_the_original_direction_independent_of_order() {
    let outbound = entry(
        "outbound",
        LedgerEntryType::Payment,
        PaymentDirection::Outbound,
        10,
        None,
    );
    let reversal = entry(
        "reversal",
        LedgerEntryType::Reversal,
        PaymentDirection::Inbound,
        10,
        Some("outbound"),
    );
    let inbound = entry("inbound", LedgerEntryType::Payment, PaymentDirection::Inbound, 7, None);
    let totals = PaymentApi::confirmed_totals(&[reversal, inbound, outbound]).expect("valid totals");
    assert_eq!(totals, (Amount::new(7), Amount::ZERO));
}

fn asset() -> AssetIdentity {
    AssetIdentity {
        network: "eip155:31337".to_owned(),
        asset: "USDC".to_owned(),
        token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
        decimals: 6,
    }
}
