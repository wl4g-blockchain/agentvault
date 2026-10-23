use crate::{GrpcConfig, VaultTransportConfig};

use super::*;

#[tokio::test]
async fn factory_creates_isolated_idempotently_closable_transports() {
    let config = VaultTransportConfig::Grpc(GrpcConfig::new("127.0.0.1:65535").with_tls(false));
    let factory = VaultTransportFactory::new(config);
    let first = factory.create().expect("first transport");
    let second = factory.create().expect("second transport");
    assert!(matches!(first, ConfiguredVaultTransport::Grpc(_)));
    assert!(matches!(second, ConfiguredVaultTransport::Grpc(_)));
    first.close().await.expect("first close");
    first.close().await.expect("idempotent close");
    second.close().await.expect("second close");
}
