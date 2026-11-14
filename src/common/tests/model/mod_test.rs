use super::*;

#[test]
fn model_facade_keeps_direction_names_protocol_neutral() {
    assert_eq!(
        serde_json::to_string(&PaymentDirection::Inbound).unwrap(),
        "\"inbound\""
    );
    assert_eq!(
        serde_json::to_string(&PaymentDirection::Outbound).unwrap(),
        "\"outbound\""
    );
}
