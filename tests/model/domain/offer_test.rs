//! `PaymentOffer` persistence-entity invariants.

use super::*;

#[test]
fn duplicate_protocols_do_not_create_ambiguous_offer_semantics() {
    let offer = PaymentOffer {
        id: "offer-1".to_owned(),
        scenario_id: "scenario-1".to_owned(),
        policy_id: "policy-1".to_owned(),
        resource: "https://merchant.example/tool".to_owned(),
        protocols: vec!["x402".to_owned(), "x402".to_owned()],
        asset: AssetIdentity {
            network: "eip155:8453".to_owned(),
            asset: "USDC".to_owned(),
            token_contract: Some("0x1111111111111111111111111111111111111111".to_owned()),
            decimals: 6,
        },
        amount: Amount::new(1),
        wallet_id: "wallet-1".to_owned(),
        enabled: true,
        created_at: 1,
        updated_at: 1,
    };
    assert_eq!(offer.validate(), Err("protocols"));
}
