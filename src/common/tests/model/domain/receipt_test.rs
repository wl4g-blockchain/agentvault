//! `PaymentReceipt` persistence-entity invariants.

use super::*;

#[test]
fn settled_receipt_requires_trusted_evidence_and_a_reference() {
    let mut receipt = PaymentReceipt {
        intent_id: "intent".to_owned(),
        scenario_id: "scenario".to_owned(),
        direction: PaymentDirection::Inbound,
        protocol: "x402".to_owned(),
        asset: AssetIdentity {
            network: "eip155:1".to_owned(),
            asset: "USDC".to_owned(),
            token_contract: None,
            decimals: 6,
        },
        recipient: "recipient".to_owned(),
        payer: Some("payer".to_owned()),
        requested_amount: Amount::new(10),
        settled_amount: Amount::new(10),
        status: PaymentStatus::Settled,
        tx_hash: None,
        settlement_ref: None,
        evidence_hash: String::new(),
        confirmed_at: 1,
    };
    assert_eq!(receipt.validate(), Err("settlement_reference"));
    receipt.tx_hash = Some("0x1".to_owned());
    receipt.evidence_hash = "evidence".to_owned();
    assert!(receipt.validate().is_ok());
}
