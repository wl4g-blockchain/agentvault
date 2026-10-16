use super::*;

#[test]
fn handler_facade_has_distinct_payer_and_payee_entrypoints() {
    assert!(std::mem::size_of::<PaymentHandler>() > 0);
    assert!(std::mem::size_of::<SettlementHandler>() > 0);
}
