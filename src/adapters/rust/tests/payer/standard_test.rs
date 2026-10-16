use crate::{IdempotencyKey, PayerRequestContext};

#[test]
fn request_context_keeps_a_stable_operation_key() {
    let first = PayerRequestContext::for_operation("run-1", "realtime-news").expect("context");
    let retry = PayerRequestContext::for_operation("run-1", "realtime-news").expect("context");
    assert_eq!(first, retry);
    assert_eq!(
        first.idempotency_key,
        IdempotencyKey::new("run-1:realtime-news").expect("key")
    );
}
