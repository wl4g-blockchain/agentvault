use super::*;
#[test]
fn connection_settings_reject_invalid_shared_consumer_group() {
    let config = crate::config::MqttTransportConfig {
        consumer_group: "invalid/group".into(),
        ..Default::default()
    };
    assert!(MqttSettings::validate_config(&config).is_err());
}
