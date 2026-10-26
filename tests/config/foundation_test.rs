use super::*;
#[test]
fn server_configuration_rejects_unknown_keys() {
    assert!(serde_yaml::from_str::<ServerConfig>("unexpected_authority: true").is_err());
    assert_eq!(ServerConfig::default().port, 8080);
}
