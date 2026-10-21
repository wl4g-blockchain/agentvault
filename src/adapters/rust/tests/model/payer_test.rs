use super::*;

#[test]
fn payer_context_and_origin_binding_are_stable() {
    let first = PayerRequestContext::new("run-1").expect("context");
    let retry = PayerRequestContext::new("run-1").expect("context");
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

#[test]
fn outbound_evidence_supports_legacy_x402_and_protocol_wires() {
    let context = OperationContext::new("scenario", "run").expect("context");
    let legacy = ReportOutboundResponseRequest::legacy(context.clone(), "legacy");
    assert!(legacy.validate().is_ok());
    let legacy = serde_json::to_value(legacy).expect("legacy JSON");
    assert_eq!(legacy["payment_response"], "legacy");
    assert!(legacy.get("protocol").is_none());

    let modern = ReportOutboundResponseRequest::protocol(context, "mpp", "payment-receipt", "receipt");
    assert!(modern.validate().is_ok());
    let modern = serde_json::to_value(modern).expect("modern JSON");
    assert_eq!(modern["protocol"], "mpp");
    assert_eq!(modern["evidence"]["header_name"], "payment-receipt");
    assert!(modern.get("payment_response").is_none());
}
