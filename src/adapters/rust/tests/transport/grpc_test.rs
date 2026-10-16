use super::*;

#[tokio::test]
async fn requires_tls_off_loopback_and_redacts_edge_credentials() {
    assert!(GrpcVaultTransport::new(GrpcConfig::new("vault.example:50051").with_tls(false)).is_err());

    let transport = GrpcVaultTransport::new(
        GrpcConfig::new("127.0.0.1:50051")
            .with_tls(false)
            .with_authguard_bearer("edge-secret"),
    )
    .expect("loopback gRPC transport");
    assert!(!format!("{transport:?}").contains("edge-secret"));
}
