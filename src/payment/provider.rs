use super::{provider_http::FixedHttpJsonTransport, provider_http::ProviderHttpEndpoint};
use crate::{
    model::SettlementOperation, model::SettlementProviderRequest, model::SettlementProviderResult, Error, Result,
};
use async_trait::async_trait;
use reqwest::header::HeaderMap;
use std::{collections::BTreeMap, collections::HashMap, sync::Arc, time::Duration};

const DEFAULT_MAX_RESPONSE_BYTES: usize = 256 * 1024;
#[async_trait]
pub trait ExternalSettlementProvider: Send + Sync {
    async fn execute(
        &self,
        operation: SettlementOperation,
        request: &SettlementProviderRequest,
    ) -> Result<SettlementProviderResult>;
}

/// Fixed protocol-to-provider routing. Challenge content cannot select or
/// override a provider endpoint.
pub struct SettlementProviderRegistry {
    providers: HashMap<String, SettlementProviderBinding>,
}

#[derive(Clone)]
pub(crate) struct SettlementProviderBinding {
    pub(crate) authority: String,
    confirmation_depths: BTreeMap<String, u64>,
    pub(crate) provider: Arc<dyn ExternalSettlementProvider>,
}

impl SettlementProviderBinding {
    pub(crate) fn confirmation_depth(&self, network: &str) -> Result<u64> {
        self.confirmation_depths.get(network).copied().ok_or_else(|| {
            Error::Settlement(format!(
                "external provider {} has no trusted confirmation depth for {network}",
                self.authority
            ))
        })
    }
}

impl SettlementProviderRegistry {
    /// Builds an immutable provider registry.
    ///
    /// # Errors
    ///
    /// Returns an error for a blank protocol or duplicate protocol
    /// registration. An empty registry represents a disabled payee direction.
    pub fn new(
        providers: impl IntoIterator<
            Item = (
                String,
                String,
                BTreeMap<String, u64>,
                Arc<dyn ExternalSettlementProvider>,
            ),
        >,
    ) -> Result<Self> {
        let mut registered = HashMap::new();
        for (protocol, authority, confirmation_depths, provider) in providers {
            if protocol.trim().is_empty()
                || authority.trim().is_empty()
                || confirmation_depths.is_empty()
                || confirmation_depths
                    .iter()
                    .any(|(network, depth)| network.trim().is_empty() || *depth == 0)
                || registered
                    .insert(
                        protocol.clone(),
                        SettlementProviderBinding {
                            authority,
                            confirmation_depths,
                            provider,
                        },
                    )
                    .is_some()
            {
                return Err(Error::Config(format!(
                    "invalid or duplicate settlement provider protocol: {protocol}"
                )));
            }
        }
        Ok(Self { providers: registered })
    }

    pub(crate) fn get(&self, protocol: &str) -> Result<SettlementProviderBinding> {
        self.providers
            .get(protocol)
            .cloned()
            .ok_or_else(|| Error::Settlement(format!("no external provider configured for {protocol}")))
    }
}

#[derive(Clone, Debug)]
pub struct SettlementProviderConfig {
    pub base_url: String,
    pub verify_path: String,
    pub settle_path: String,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    /// Plain HTTP is accepted only for a loopback provider and only when this
    /// development switch is explicit.
    pub allow_insecure_loopback: bool,
}

impl Default for SettlementProviderConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            verify_path: "verify".to_owned(),
            settle_path: "settle".to_owned(),
            timeout: Duration::from_secs(10),
            max_response_bytes: DEFAULT_MAX_RESPONSE_BYTES,
            allow_insecure_loopback: false,
        }
    }
}

/// Redirect-free HTTP adapter for hosted facilitators and providers.
///
/// Paths are fixed at construction time. Credentials are therefore never
/// forwarded to a URL supplied by a payment challenge.
pub struct HttpSettlementProvider {
    transport: FixedHttpJsonTransport,
}

impl HttpSettlementProvider {
    /// Builds a provider client with an optional pre-validated secret header
    /// map. Header values are intentionally not retained in configuration
    /// models that may be serialized or logged.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe endpoint, invalid fixed paths, zero
    /// limits, or an HTTP client construction failure.
    pub fn new(config: &SettlementProviderConfig, headers: HeaderMap) -> Result<Self> {
        Ok(Self {
            transport: FixedHttpJsonTransport::new(config, headers)?,
        })
    }
}

#[async_trait]
impl ExternalSettlementProvider for HttpSettlementProvider {
    async fn execute(
        &self,
        operation: SettlementOperation,
        request: &SettlementProviderRequest,
    ) -> Result<SettlementProviderResult> {
        let endpoint = match operation {
            SettlementOperation::Verify => ProviderHttpEndpoint::Verify,
            SettlementOperation::Settle => ProviderHttpEndpoint::Settle,
        };
        let response = self.transport.post_json(endpoint, &request.run_id, request).await?;
        if !response.status.is_success() {
            return Err(Error::Settlement(format!("provider returned HTTP {}", response.status)));
        }
        serde_json::from_slice(&response.body)
            .map_err(|error| Error::Settlement(format!("invalid provider response: {error}")))
    }
}

#[cfg(test)]
#[path = "../../tests/payment/provider_test.rs"]
mod tests;
