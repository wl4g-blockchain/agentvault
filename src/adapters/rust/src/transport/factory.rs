use std::fmt;

use crate::{
    GrpcConfig, GrpcVaultTransport, MqttConfig, MqttVaultTransport, TransportError, TransportRequest,
    TransportResponse, VaultTransport,
};

#[derive(Clone, Debug)]
pub enum VaultTransportConfig {
    Grpc(GrpcConfig),
    Mqtt(MqttConfig),
}

#[derive(Clone)]
pub enum ConfiguredVaultTransport {
    Grpc(GrpcVaultTransport),
    Mqtt(MqttVaultTransport),
}

impl fmt::Debug for ConfiguredVaultTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Grpc(transport) => formatter.debug_tuple("Grpc").field(transport).finish(),
            Self::Mqtt(transport) => formatter.debug_tuple("Mqtt").field(transport).finish(),
        }
    }
}

impl VaultTransport for ConfiguredVaultTransport {
    async fn send(&self, request: TransportRequest) -> std::result::Result<TransportResponse, TransportError> {
        match self {
            Self::Grpc(transport) => transport.send(request).await,
            Self::Mqtt(transport) => transport.send(request).await,
        }
    }

    async fn close(&self) -> std::result::Result<(), TransportError> {
        match self {
            Self::Grpc(transport) => transport.close().await,
            Self::Mqtt(transport) => transport.close().await,
        }
    }
}

/// Typed transport factory. Every call constructs an independent provider;
/// failures and connections are never cached across clients.
#[derive(Clone, Debug)]
pub struct VaultTransportFactory {
    config: VaultTransportConfig,
}

impl VaultTransportFactory {
    #[must_use]
    pub const fn new(config: VaultTransportConfig) -> Self {
        Self { config }
    }

    pub fn create(&self) -> std::result::Result<ConfiguredVaultTransport, TransportError> {
        match &self.config {
            VaultTransportConfig::Grpc(config) => {
                GrpcVaultTransport::new(config.clone()).map(ConfiguredVaultTransport::Grpc)
            }
            VaultTransportConfig::Mqtt(config) => {
                MqttVaultTransport::new(config.clone()).map(ConfiguredVaultTransport::Mqtt)
            }
        }
    }
}

#[cfg(test)]
#[path = "../../tests/transport/factory_test.rs"]
mod tests;
