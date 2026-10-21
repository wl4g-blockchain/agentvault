use reqwest::header::{HeaderMap, HeaderValue, WWW_AUTHENTICATE};

use crate::PayerRequestContext;
use serde_json::json;

#[test]
fn request_context_keeps_a_stable_run_id() {
    let first = PayerRequestContext::new("run-1").expect("context");
    let retry = PayerRequestContext::new("run-1").expect("context");
    assert_eq!(first, retry);
    assert_eq!(first.run_id, "run-1");
}

#[test]
fn repeated_authentication_challenges_are_preserved_for_protocol_selection() {
    let mut headers = HeaderMap::new();
    headers.append(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer realm=\"login\""));
    headers.append(
        WWW_AUTHENTICATE,
        HeaderValue::from_static("Payment id=\"challenge\", method=\"tempo\", intent=\"charge\""),
    );

    let flattened = super::request_headers(&headers).expect("text headers");

    assert_eq!(
        flattened.get("www-authenticate").map(String::as_str),
        Some("Bearer realm=\"login\", Payment id=\"challenge\", method=\"tempo\", intent=\"charge\"")
    );
}

#[test]
fn unrelated_binary_response_header_does_not_erase_payment_receipt() {
    let mut headers = HeaderMap::new();
    headers.insert("payment-receipt", HeaderValue::from_static("receipt-1"));
    headers.insert(
        "x-binary-metadata",
        HeaderValue::from_bytes(&[0x80]).expect("opaque response header"),
    );

    let flattened = super::response_headers(&headers);

    assert_eq!(flattened.get("payment-receipt").map(String::as_str), Some("receipt-1"));
    assert!(!flattened.contains_key("x-binary-metadata"));
}

#[test]
fn mcp_envelopes_are_bound_to_jsonrpc_version_and_request_id() {
    let id = json!(7);
    assert!(super::ReqwestDelivery::bound_mcp_response(
        br#"{"jsonrpc":"2.0","id":7,"result":{}}"#,
        &id,
        true,
    ));
    assert!(!super::ReqwestDelivery::bound_mcp_response(
        br#"{"jsonrpc":"2.0","id":8,"result":{}}"#,
        &id,
        true,
    ));
    assert!(!super::ReqwestDelivery::bound_mcp_response(
        br#"{"jsonrpc":"1.0","id":7,"result":{}}"#,
        &id,
        true,
    ));
}
