use crate::PayerRequestContext;

#[test]
fn request_context_keeps_a_stable_run_id() {
    let first = PayerRequestContext::new("run-1").expect("context");
    let retry = PayerRequestContext::new("run-1").expect("context");
    assert_eq!(first, retry);
    assert_eq!(first.run_id, "run-1");
}
