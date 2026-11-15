use super::*;

#[test]
fn signing_request_round_trips_without_protocol_specific_core_fields() {
    let request = ProtocolSigningRequest {
        digest: vec![7; 32],
        payload: serde_json::json!({"authorization": "opaque"}),
    };
    let encoded = serde_json::to_vec(&request).unwrap();
    assert_eq!(
        serde_json::from_slice::<ProtocolSigningRequest>(&encoded).unwrap(),
        request
    );
}
