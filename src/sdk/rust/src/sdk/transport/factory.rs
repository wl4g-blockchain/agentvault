use crate::{HttpConfig, HttpVaultTransport, TransportError};
#[derive(Clone, Debug)]
pub enum VaultTransportConfig {
    Http(HttpConfig),
}
pub type ConfiguredVaultTransport = HttpVaultTransport;
#[derive(Clone, Debug)]
pub struct VaultTransportFactory {
    config: VaultTransportConfig,
}
impl VaultTransportFactory {
    #[must_use]
    pub const fn new(config: VaultTransportConfig) -> Self {
        Self { config }
    }
    pub fn create(&self) -> Result<ConfiguredVaultTransport, TransportError> {
        match &self.config {
            VaultTransportConfig::Http(config) => HttpVaultTransport::new(config.clone()),
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/transport/factory_test.rs"]
mod tests;
