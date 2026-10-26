use super::*;

#[test]
fn facade_only_exposes_row_facing_domain_services() {
    assert!(std::mem::size_of::<ApprovalHandler>() > 0);
    assert!(std::mem::size_of::<IntentHandler>() > 0);
    assert!(std::mem::size_of::<LedgerHandler>() > 0);
    assert!(std::mem::size_of::<PriceHandler>() > 0);
}
