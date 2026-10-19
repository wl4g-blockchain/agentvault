use super::*;

#[test]
fn facilitator_wire_rejects_unknown_fields_fail_closed() {
    let response = br#"{"isValid":true,"payer":"0x1","unexpected":true}"#;
    assert!(parse_verify_response(response).is_err());
}

#[test]
fn facilitator_wire_accepts_the_official_success_shape() {
    let response = br#"{"isValid":true,"payer":"0x0000000000000000000000000000000000000001"}"#;
    assert!(parse_verify_response(response).is_ok());
}
