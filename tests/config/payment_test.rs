use super::*;
#[test]
fn payer_configuration_does_not_accept_payee_provider_authority() {
    assert!(serde_yaml::from_str::<PayerConfig>("facilitator: https://untrusted.example").is_err());
}

#[test]
fn removed_unix_socket_transport_is_rejected_as_unknown_configuration() {
    let error = serde_yaml::from_str::<TransportConfig>("local: { enabled: true, socket_path: /tmp/agentvault.sock }")
        .unwrap_err();
    assert!(error.to_string().contains("unknown field `local`"));
    let transport =
        serde_yaml::from_str::<TransportConfig>("grpc: { enabled: true }\nmqtt: { enabled: false }").unwrap();
    assert!(transport.grpc.enabled);
    assert!(!transport.mqtt.enabled);
}
