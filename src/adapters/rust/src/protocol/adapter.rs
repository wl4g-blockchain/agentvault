use std::{
    collections::BTreeMap,
    sync::{Arc, RwLock},
};

use serde_json::Value;

use crate::{Error, HttpCredential, PaymentChallenge, PaymentCredential, Result, SettledIncomingPayment};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProtocolHttpResponse {
    pub headers: BTreeMap<String, String>,
    pub content_type: Option<String>,
    pub body: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MerchantEvidence {
    pub header_name: String,
    pub header_value: String,
}

/// One protocol-owned MCP `_meta` entry. Framework adapters preserve every
/// unrelated metadata key and reject an attempt to overwrite this key.
#[derive(Clone, Debug, PartialEq)]
pub struct ProtocolMcpMetadata {
    pub key: &'static str,
    pub value: Value,
}

/// Protocol-defined payment-required shape inside a JSON-RPC response.
/// x402 uses an MCP tool result while MPP uses a JSON-RPC error object.
#[derive(Clone, Debug, PartialEq)]
pub enum ProtocolMcpPaymentRequired {
    Result(Value),
    Error(Value),
}

/// HTTP wire mapping only. Wallet keys, policy, signing, and authoritative
/// settlement remain inside `AgentVault`.
pub trait PaymentProtocol: Send + Sync {
    fn name(&self) -> &'static str;
    /// HTTP request headers that can carry this protocol's credential. The
    /// framework uses this only to preserve duplicates and delegates parsing
    /// (including schemes such as `Authorization: Payment`) back here.
    fn credential_header_names(&self) -> &'static [&'static str] {
        &[]
    }
    /// HTTP response headers owned by this protocol's settlement evidence.
    /// Framework adapters use the names to reject business-handler forgery;
    /// parsing and trust decisions remain with `merchant_evidence` and Vault.
    fn merchant_evidence_header_names(&self) -> &'static [&'static str] {
        &[]
    }
    fn credential(&self, credential: &PaymentCredential) -> Result<HttpCredential>;
    fn merchant_evidence(&self, headers: &BTreeMap<String, String>) -> Result<Option<MerchantEvidence>>;
    fn presented_credential(&self, headers: &BTreeMap<String, String>, limit: usize) -> Result<Option<Value>>;
    fn challenge(&self, challenge: &PaymentChallenge) -> Result<ProtocolHttpResponse>;
    fn receipt(&self, payment: &SettledIncomingPayment) -> Result<ProtocolHttpResponse>;

    /// Derives an optional challenge deadline from protocol-native data. The
    /// generic payer workflow never interprets protocol fields itself.
    fn challenge_expires_at(&self, _headers: &BTreeMap<String, String>, _body: &str) -> Result<Option<String>> {
        Ok(None)
    }

    /// Recognizes a protocol payment challenge in one complete JSON-RPC
    /// response and projects it onto the existing HTTP capture contract used
    /// by `AgentVault`. `None` means the message is unrelated to this protocol.
    fn mcp_challenge(&self, _message: &Value, _limit: usize) -> Result<Option<ProtocolHttpResponse>> {
        Ok(None)
    }

    /// Maps a Vault-issued credential to the protocol's official MCP `_meta`
    /// key and value.
    fn mcp_credential(&self, _credential: &PaymentCredential) -> Result<ProtocolMcpMetadata> {
        Err(Error::InvalidArgument(format!(
            "payment protocol does not support MCP credential delivery: {}",
            self.name()
        )))
    }

    fn mcp_request_metadata_key(&self) -> Option<&'static str> {
        None
    }

    /// Optional JSON-RPC root `_meta` placement accepted in addition to MCP's
    /// nested `params._meta`. Only protocols whose wire specification permits
    /// both locations should expose a key here.
    fn mcp_root_request_metadata_key(&self) -> Option<&'static str> {
        None
    }

    /// Decodes an MCP request `_meta` object into the payload already accepted
    /// by the Vault verification endpoint.
    fn presented_mcp_credential(&self, _meta: &Value, _limit: usize) -> Result<Option<Value>> {
        Ok(None)
    }

    /// Builds the protocol's official payment-required MCP result/error value.
    fn mcp_payment_required(&self, _challenge: &PaymentChallenge) -> Result<ProtocolMcpPaymentRequired> {
        Err(Error::InvalidArgument(format!(
            "payment protocol does not support MCP challenges: {}",
            self.name()
        )))
    }

    /// Reissues a fresh protocol challenge after Vault rejected an MCP
    /// credential. Providers retain their native failure wire shape.
    fn mcp_payment_rejected(
        &self,
        _challenge: &PaymentChallenge,
        _message: &str,
    ) -> Result<ProtocolMcpPaymentRequired> {
        Err(Error::InvalidArgument(format!(
            "payment protocol does not support MCP verification failures: {}",
            self.name()
        )))
    }

    /// Builds the protocol's official successful MCP receipt metadata.
    fn mcp_receipt(
        &self,
        _challenge: &PaymentChallenge,
        _payment: &SettledIncomingPayment,
    ) -> Result<ProtocolMcpMetadata> {
        Err(Error::InvalidArgument(format!(
            "payment protocol does not support MCP receipts: {}",
            self.name()
        )))
    }

    fn mcp_receipt_metadata_key(&self) -> Option<&'static str> {
        None
    }

    /// Converts untrusted merchant MCP metadata to the existing protocolized
    /// evidence envelope. `AgentVault` still performs authoritative recovery and
    /// chain/provider confirmation before posting the ledger.
    fn mcp_merchant_evidence(&self, _meta: &Value, _limit: usize) -> Result<Option<MerchantEvidence>> {
        Ok(None)
    }

    fn header<'a>(&self, headers: &'a BTreeMap<String, String>, wanted: &str) -> Option<&'a str> {
        headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.as_str())
    }
}

#[derive(Clone)]
pub struct ProtocolRegistry {
    adapters: BTreeMap<String, Arc<dyn PaymentProtocol>>,
    priority: Vec<String>,
}

impl ProtocolRegistry {
    pub fn get(&self, name: &str) -> Result<&dyn PaymentProtocol> {
        self.adapters
            .get(name)
            .map(Arc::as_ref)
            .ok_or_else(|| Error::InvalidArgument(format!("payment protocol is not enabled: {name}")))
    }

    /// Resolves a message-level MCP challenge in configured protocol order.
    pub fn mcp_challenge(
        &self,
        message: &Value,
        limit: usize,
    ) -> Result<Option<(&dyn PaymentProtocol, ProtocolHttpResponse)>> {
        for name in &self.priority {
            let adapter = self.get(name)?;
            if let Some(challenge) = adapter.mcp_challenge(message, limit)? {
                return Ok(Some((adapter, challenge)));
            }
        }
        Ok(None)
    }

    #[must_use]
    pub fn credential_header_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = Vec::new();
        for name in &self.priority {
            if let Ok(adapter) = self.get(name) {
                for &header in adapter.credential_header_names() {
                    if !names.iter().any(|existing| existing.eq_ignore_ascii_case(header)) {
                        names.push(header);
                    }
                }
            }
        }
        names
    }

    #[must_use]
    pub fn merchant_evidence_header_names(&self) -> Vec<&'static str> {
        let mut names: Vec<&'static str> = Vec::new();
        for name in &self.priority {
            if let Ok(adapter) = self.get(name) {
                for &header in adapter.merchant_evidence_header_names() {
                    if !names.iter().any(|existing| existing.eq_ignore_ascii_case(header)) {
                        names.push(header);
                    }
                }
            }
        }
        names
    }

    pub fn presented_credential(
        &self,
        headers: &BTreeMap<String, String>,
        limit: usize,
    ) -> Result<Option<(String, Value)>> {
        let mut selected = None;
        for name in &self.priority {
            let adapter = self.get(name)?;
            let Some(value) = adapter.presented_credential(headers, limit)? else {
                continue;
            };
            if selected.is_some() {
                return Err(Error::InvalidArgument(
                    "multiple payment protocol credentials were supplied".to_owned(),
                ));
            }
            selected = Some((name.clone(), value));
        }
        Ok(selected)
    }

    pub fn presented_mcp_credential(&self, meta: &Value, limit: usize) -> Result<Option<(String, Value)>> {
        let mut selected = None;
        for name in &self.priority {
            let adapter = self.get(name)?;
            let Some(value) = adapter.presented_mcp_credential(meta, limit)? else {
                continue;
            };
            if selected.is_some() {
                return Err(Error::InvalidArgument(
                    "multiple MCP payment credentials were supplied".to_owned(),
                ));
            }
            selected = Some((name.clone(), value));
        }
        Ok(selected)
    }

    pub fn merchant_evidence(&self, headers: &BTreeMap<String, String>) -> Result<Option<(String, MerchantEvidence)>> {
        let mut selected = None;
        for name in &self.priority {
            let adapter = self.get(name)?;
            let Some(value) = adapter.merchant_evidence(headers)? else {
                continue;
            };
            if selected.is_some() {
                return Err(Error::InvalidArgument(
                    "multiple payment protocol receipts were supplied".to_owned(),
                ));
            }
            selected = Some((name.clone(), value));
        }
        Ok(selected)
    }

    pub fn mcp_merchant_evidence(&self, meta: &Value, limit: usize) -> Result<Option<(String, MerchantEvidence)>> {
        let mut selected = None;
        for name in &self.priority {
            let adapter = self.get(name)?;
            let Some(value) = adapter.mcp_merchant_evidence(meta, limit)? else {
                continue;
            };
            if selected.is_some() {
                return Err(Error::InvalidArgument(
                    "multiple MCP settlement receipts were supplied".to_owned(),
                ));
            }
            selected = Some((name.clone(), value));
        }
        Ok(selected)
    }

    #[must_use]
    pub fn mcp_request_metadata_keys(&self) -> Vec<&'static str> {
        self.priority
            .iter()
            .filter_map(|name| self.get(name).ok()?.mcp_request_metadata_key())
            .collect()
    }

    #[must_use]
    pub fn mcp_root_request_metadata_keys(&self) -> Vec<&'static str> {
        self.priority
            .iter()
            .filter_map(|name| self.get(name).ok()?.mcp_root_request_metadata_key())
            .collect()
    }

    #[must_use]
    pub fn mcp_receipt_metadata_keys(&self) -> Vec<&'static str> {
        self.priority
            .iter()
            .filter_map(|name| self.get(name).ok()?.mcp_receipt_metadata_key())
            .collect()
    }
}

type ProtocolProvider = Arc<dyn Fn() -> Arc<dyn PaymentProtocol> + Send + Sync>;

/// Concurrency-safe, instance-scoped provider factory. Each `create` call
/// constructs a fresh protocol registry and never caches a partial failure.
#[derive(Clone)]
pub struct PaymentProtocolFactory {
    providers: Arc<RwLock<BTreeMap<String, ProtocolProvider>>>,
}

impl Default for PaymentProtocolFactory {
    fn default() -> Self {
        let providers = BTreeMap::from([
            (
                "mpp".to_owned(),
                Arc::new(|| Arc::new(crate::MppAdapter) as Arc<dyn PaymentProtocol>) as ProtocolProvider,
            ),
            (
                "x402".to_owned(),
                Arc::new(|| Arc::new(crate::X402Adapter) as Arc<dyn PaymentProtocol>) as ProtocolProvider,
            ),
        ]);
        Self {
            providers: Arc::new(RwLock::new(providers)),
        }
    }
}

impl PaymentProtocolFactory {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<F>(&self, name: impl Into<String>, provider: F) -> Result<()>
    where
        F: Fn() -> Arc<dyn PaymentProtocol> + Send + Sync + 'static,
    {
        let name = name.into();
        if name.is_empty()
            || name != name.to_ascii_lowercase()
            || !name
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        {
            return Err(Error::InvalidArgument(
                "payment protocol provider name is invalid".to_owned(),
            ));
        }
        let mut providers = self
            .providers
            .write()
            .map_err(|_| Error::InvalidArgument("payment protocol factory lock is poisoned".to_owned()))?;
        if providers.contains_key(&name) {
            return Err(Error::InvalidArgument(format!(
                "payment protocol provider is already registered: {name}"
            )));
        }
        providers.insert(name, Arc::new(provider));
        Ok(())
    }

    pub fn create(&self, enabled: &[String]) -> Result<ProtocolRegistry> {
        if enabled.is_empty() {
            return Err(Error::InvalidArgument(
                "payment protocol configuration is empty".to_owned(),
            ));
        }
        let providers = self
            .providers
            .read()
            .map_err(|_| Error::InvalidArgument("payment protocol factory lock is poisoned".to_owned()))?;
        let mut adapters = BTreeMap::new();
        let mut priority = Vec::with_capacity(enabled.len());
        for name in enabled {
            if adapters.contains_key(name) {
                return Err(Error::InvalidArgument(format!(
                    "duplicate payment protocol provider: {name}"
                )));
            }
            let provider = providers
                .get(name)
                .ok_or_else(|| Error::InvalidArgument(format!("payment protocol provider is unavailable: {name}")))?;
            let adapter = provider();
            if adapter.name() != name {
                return Err(Error::InvalidArgument(format!(
                    "payment protocol provider identity mismatch: {name}"
                )));
            }
            priority.push(name.clone());
            adapters.insert(name.clone(), adapter);
        }
        Ok(ProtocolRegistry { adapters, priority })
    }
}

#[cfg(test)]
#[path = "../../tests/protocol/adapter_test.rs"]
mod tests;
