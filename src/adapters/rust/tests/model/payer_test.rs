use super::*;

#[test]
fn payer_context_and_origin_binding_are_stable() {
    let first = PayerRequestContext::for_operation("run-1", "realtime").expect("context");
    let retry = PayerRequestContext::for_operation("run-1", "realtime").expect("context");
    assert_eq!(first, retry);
    assert!(http_url_matches_origin(
        "https://merchant.test/mcp",
        "https://merchant.test"
    ));
    assert!(!http_url_matches_origin(
        "https://merchant.test.evil/mcp",
        "https://merchant.test"
    ));
}
