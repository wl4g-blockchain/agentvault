use super::*;

#[test]
fn http_component_uses_the_configured_service_boundary() {
    let authguard = crate::config::AuthGuardIntegrationConfig {
        enabled: false,
        ..Default::default()
    };
    let server = HttpApiServer::new(
        crate::config::ServerConfig::default(),
        PrincipalContext::from_config(&authguard).unwrap(),
        Router::new(),
        AgentVaultMetrics::default(),
    );
    assert_eq!(RuntimeComponent::name(&server), "HTTP API");
}
