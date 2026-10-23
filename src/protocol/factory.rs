use std::{collections::BTreeMap, collections::HashMap, sync::Arc};

use serde_json::Value;

use crate::{Error, Result};

use super::PaymentProtocol;

/// Immutable, direction-scoped registry of configured payment protocols.
///
/// A payer and a payee receive separate registry instances so enabling a
/// protocol for one role never grants it to the other role implicitly.
pub struct ProtocolRegistry {
    adapters: HashMap<&'static str, Arc<dyn PaymentProtocol>>,
    priority: Vec<&'static str>,
}

impl ProtocolRegistry {
    /// Resolves a configured protocol by its stable wire name.
    ///
    /// # Errors
    ///
    /// Returns an error when the protocol is not enabled for this registry's
    /// role.
    pub fn get(&self, protocol: &str) -> Result<Arc<dyn PaymentProtocol>> {
        self.adapters
            .get(protocol)
            .cloned()
            .ok_or_else(|| Error::PaymentProtocol(format!("unsupported payment protocol: {protocol}")))
    }

    #[must_use]
    pub fn contains(&self, protocol: &str) -> bool {
        self.adapters.contains_key(protocol)
    }

    /// Resolves one HTTP challenge by explicit hint or configured provider
    /// priority. A provider returns `None` for unrelated authentication
    /// schemes, so Bearer/Basic challenges are never treated as payments.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported hint, malformed protocol wire, or
    /// a response that contains no enabled payment challenge.
    pub fn resolve_http_challenge(
        &self,
        protocol_hint: Option<&str>,
        resource: &str,
        headers: &BTreeMap<String, String>,
        body: &str,
    ) -> Result<(Arc<dyn PaymentProtocol>, Value)> {
        if let Some(protocol) = protocol_hint {
            if protocol != protocol.trim() {
                return Err(Error::PaymentProtocol(
                    "payment protocol hint is not canonical".to_owned(),
                ));
            }
            let adapter = self.get(protocol)?;
            let wire = adapter
                .decode_http_challenge(resource, headers, body)?
                .ok_or_else(|| Error::PaymentProtocol(format!("HTTP response did not advertise {protocol}")))?;
            return Ok((adapter, wire));
        }
        for protocol in &self.priority {
            let adapter = self.get(protocol)?;
            if let Some(wire) = adapter.decode_http_challenge(resource, headers, body)? {
                return Ok((adapter, wire));
            }
        }
        Err(Error::PaymentProtocol(
            "HTTP response contains no enabled payment challenge".to_owned(),
        ))
    }
}

/// Builds one isolated protocol registry. Failed registrations are never
/// cached and duplicate names fail closed.
#[derive(Default)]
pub struct PaymentProtocolFactory {
    adapters: Vec<Arc<dyn PaymentProtocol>>,
}

impl PaymentProtocolFactory {
    #[must_use]
    pub const fn new() -> Self {
        Self { adapters: Vec::new() }
    }

    #[must_use]
    pub fn register(mut self, adapter: Arc<dyn PaymentProtocol>) -> Self {
        self.adapters.push(adapter);
        self
    }

    /// Builds a registry and rejects duplicate adapter names.
    ///
    /// # Errors
    ///
    /// Returns an error when two configured providers claim the same protocol
    /// name. An empty registry represents a disabled payment direction.
    pub fn build(self) -> Result<ProtocolRegistry> {
        let mut registered = HashMap::new();
        let mut priority = Vec::with_capacity(self.adapters.len());
        for adapter in self.adapters {
            let name = adapter.name();
            if registered.insert(name, adapter).is_some() {
                return Err(Error::Config(format!("duplicate payment protocol adapter: {name}")));
            }
            priority.push(name);
        }
        Ok(ProtocolRegistry {
            adapters: registered,
            priority,
        })
    }
}

impl ProtocolRegistry {
    /// Compatibility constructor retained for existing embedders. New runtime
    /// assembly should use [`PaymentProtocolFactory`].
    ///
    /// # Errors
    ///
    /// Returns an error when two adapters claim the same protocol name.
    pub fn new(adapters: impl IntoIterator<Item = Arc<dyn PaymentProtocol>>) -> Result<Self> {
        adapters
            .into_iter()
            .fold(PaymentProtocolFactory::new(), PaymentProtocolFactory::register)
            .build()
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/factory_test.rs"]
mod tests;
