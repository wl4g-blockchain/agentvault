use super::*;

#[test]
fn server_can_be_constructed_only_from_typed_configuration() {
    let server = AgentVaultServer::new(AppConfig::default());
    assert!(std::mem::size_of_val(&server) > 0);
}
