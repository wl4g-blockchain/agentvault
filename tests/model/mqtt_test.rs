use super::*;
#[test]
fn response_uses_the_versioned_camel_case_wire_contract() {
    let response = ResponseEnvelope {
        message_type: "payment.error".into(),
        version: 1,
        request_span_id: "0123456789abcdef".into(),
        status: 400,
        headers: BTreeMap::new(),
        body: "e30".into(),
    };
    let value = serde_json::to_value(&response).unwrap();
    assert_eq!(value["requestSpanId"], "0123456789abcdef");
    assert_eq!(value["type"], "payment.error");
    assert_eq!(value["version"], 1);
    assert!(value.get("request_span_id").is_none());
}
