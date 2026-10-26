use super::*;
#[test]
fn durations_convert_to_bounded_database_seconds() {
    assert_eq!(
        PaymentRuntimeFactory::seconds(std::time::Duration::from_secs(17), "interval").unwrap(),
        17
    );
}
