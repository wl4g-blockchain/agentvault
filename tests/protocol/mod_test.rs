use super::*;

#[test]
fn protocol_validation_defaults_to_local_only() {
    let validation = ProtocolValidation::default();
    assert!(validation.payer.is_none());
    assert!(!validation.requires_external_settlement);
}
