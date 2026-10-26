use crate::{model::Amount, storage::payment::PaymentStorageRules};
#[test]
fn adding_confirmed_usage_never_wraps_the_budget() {
    assert!(PaymentStorageRules::checked_usage(Amount::new(u128::MAX), Amount::new(1), "wallet").is_err());
    assert_eq!(
        PaymentStorageRules::checked_usage(Amount::new(2), Amount::new(3), "wallet").unwrap(),
        Amount::new(5)
    );
}
