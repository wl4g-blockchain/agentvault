use super::*;
#[test]
fn absence_can_expire_an_authorization_only_after_both_clocks_pass_the_deadline() {
    assert!(!SettlementFacts::authorization_is_definitively_expired(100, 99, 100));
    assert!(!SettlementFacts::authorization_is_definitively_expired(99, 100, 100));
    assert!(SettlementFacts::authorization_is_definitively_expired(100, 100, 100));
}
