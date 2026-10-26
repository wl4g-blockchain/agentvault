use super::*;

#[test]
fn domain_facade_exposes_authoritative_persistence_entities() {
    assert!(std::mem::size_of::<PaymentIntent>() > 0);
    assert!(std::mem::size_of::<PaymentLedgerEntry>() > 0);
    assert!(std::mem::size_of::<Wallet>() > 0);
}
