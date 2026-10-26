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
fn outbound_evidence_requires_protocol_receipts() {
    let context = OperationContext::new("scenario", "run").expect("context");
    for (protocol, header) in [("x402", "payment-response"), ("mpp", "payment-receipt")] {
        let request = ReportOutboundResponseRequest::new(context.clone(), protocol, header, "receipt");
        assert!(request.validate().is_ok());
        let wire = serde_json::to_value(request).expect("JSON");
        assert_eq!(wire["protocol"], protocol);
        assert_eq!(wire["evidence"]["header_name"], header);
        assert!(wire.get("payment_response").is_none());
    }
    assert!(
        serde_json::from_value::<ReportOutboundResponseRequest>(serde_json::json!({
            "scenario_id": "scenario", "run_id": "run", "payment_response": "receipt"
        }))
        .is_err()
    );
}
