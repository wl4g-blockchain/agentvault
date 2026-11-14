use super::*;

#[test]
fn delivery_and_dispatch_results_keep_failure_classes_distinct() {
    assert_ne!(
        PaymentEventDelivery::Retryable {
            reason: "temporary".to_owned(),
        },
        PaymentEventDelivery::PermanentFailure {
            reason: "temporary".to_owned(),
        }
    );
    assert_eq!(PaymentEventDispatchRun::default().claimed, 0);
}
