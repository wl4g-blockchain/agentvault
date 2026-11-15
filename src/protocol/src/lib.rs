pub use agentvault_common::{apm, config, model, storage, util, Error, Result};
// Payment protocol adapter boundary.

pub mod mpp;
pub mod x402;

#[cfg(test)]
#[path = "../tests/mod_test.rs"]
mod tests;

use std::{collections::BTreeMap, collections::HashMap, sync::Arc};

use serde_json::Value;

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

#[cfg(test)]
#[path = "../tests/factory_test.rs"]
mod factory_tests;

pub mod receipt;
pub mod rpc;
mod contract {
    use crate::ProtocolSigningRequest;
    use std::collections::BTreeMap;

    use async_trait::async_trait;
    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    use crate::{
        model::PaymentChallenge, model::PaymentCredential, model::PaymentIntent, model::PaymentPrice,
        model::PaymentReceipt, Error, Result,
    };

    /// Result of local structural and binding validation. Cryptographic/provider
    /// confirmation remains a separate settlement step.
    #[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct ProtocolValidation {
        #[serde(skip_serializing_if = "Option::is_none")]
        pub payer: Option<String>,
        pub requires_external_settlement: bool,
    }

    /// Protocol-specific material sent to an external facilitator/provider.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    pub struct ProtocolSettlement {
        pub protocol: String,
        pub payment_payload: Value,
        pub payment_requirements: Value,
    }

    /// Protocol-owned HTTP credential mapping. SDKs apply it only to the
    /// challenge-bound origin and never overwrite unrelated authentication.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct ProtocolHttpCredential {
        pub header_name: String,
        pub header_value: String,
    }

    /// Untrusted merchant settlement hint decoded by the protocol provider.
    /// Ledger state still requires independent chain/provider confirmation.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
    #[serde(deny_unknown_fields)]
    pub struct ProtocolMerchantEvidence {
        pub accepted: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub transaction: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub network: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub payer: Option<String>,
    }

    /// Bidirectional protocol adapter. It translates protocol wire structures into
    /// the shared payment domain and never owns policy, keys, or financial state.
    #[async_trait]
    pub trait PaymentProtocol: Send + Sync {
        fn name(&self) -> &'static str;

        /// Recognizes and decodes this protocol's HTTP 402 representation. `None`
        /// means the response did not advertise this protocol.
        ///
        /// # Errors
        ///
        /// Returns an error when the response declares this protocol with invalid
        /// or conflicting wire data.
        fn decode_http_challenge(
            &self,
            _resource: &str,
            _headers: &BTreeMap<String, String>,
            _body: &str,
        ) -> Result<Option<Value>> {
            Ok(None)
        }

        /// Maps a canonical credential to its protocol-defined request header.
        ///
        /// # Errors
        ///
        /// Returns an error when the credential is not valid for this protocol's
        /// HTTP delivery contract.
        fn encode_http_credential(&self, _credential: &PaymentCredential) -> Result<ProtocolHttpCredential> {
            Err(Error::PaymentProtocol(
                "HTTP credential mapping is not implemented for this payment method".to_owned(),
            ))
        }

        /// Protocol-defined successful response header name.
        fn merchant_evidence_header(&self) -> &'static str {
            ""
        }

        /// Decodes merchant response material as an untrusted settlement hint.
        ///
        /// # Errors
        ///
        /// Returns an error when the evidence is malformed or is not a successful
        /// receipt for this protocol.
        fn decode_merchant_evidence(&self, _value: &str) -> Result<ProtocolMerchantEvidence> {
            Err(Error::PaymentProtocol(
                "merchant evidence is not implemented for this payment method".to_owned(),
            ))
        }

        /// Derives the canonical payer from a signed credential without trusting
        /// merchant response metadata.
        ///
        /// # Errors
        ///
        /// Returns an error when the challenge, credential, or signer binding is
        /// invalid.
        fn credential_payer(&self, _challenge: &PaymentChallenge, _credential: &PaymentCredential) -> Result<String> {
            Err(Error::PaymentProtocol(
                "credential payer extraction is not implemented for this payment method".to_owned(),
            ))
        }

        // Payer
        async fn parse_challenge(
            &self,
            scenario_id: &str,
            expected_resource: &str,
            wire: &Value,
            now: i64,
            advertised_expires_at: Option<i64>,
        ) -> Result<PaymentChallenge>;

        async fn validate_payment(&self, challenge: &PaymentChallenge, intent: &PaymentIntent) -> Result<()>;

        async fn build_credential(
            &self,
            challenge: &PaymentChallenge,
            signed_payload: Value,
        ) -> Result<PaymentCredential>;

        /// Builds the exact protocol payload and digest that a local wallet may
        /// sign after authorization and balance checks have succeeded.
        async fn prepare_signing(
            &self,
            _challenge: &PaymentChallenge,
            _payer: &str,
            _now: i64,
        ) -> Result<ProtocolSigningRequest> {
            Err(Error::PaymentProtocol(
                "payer signing is not implemented for this payment method".to_owned(),
            ))
        }

        /// Inserts a recoverable wallet signature into the adapter-owned payload
        /// and returns the canonical payment credential.
        async fn complete_signing(
            &self,
            _challenge: &PaymentChallenge,
            _request: ProtocolSigningRequest,
            _signature: &[u8],
        ) -> Result<PaymentCredential> {
            Err(Error::PaymentProtocol(
                "payer signing is not implemented for this payment method".to_owned(),
            ))
        }

        async fn verify_receipt(&self, intent: &PaymentIntent, receipt: &PaymentReceipt) -> Result<()>;

        // Payee
        async fn build_challenge(&self, price: &PaymentPrice, recipient: &str, now: i64) -> Result<PaymentChallenge>;

        async fn verify_credential(
            &self,
            challenge: &PaymentChallenge,
            credential: &PaymentCredential,
            now: i64,
        ) -> Result<ProtocolValidation>;

        /// Returns the protocol-defined replay identity after structural
        /// validation. Implementations must derive this from canonical payment
        /// semantics rather than caller-provided hashes or raw JSON encoding.
        ///
        /// # Errors
        ///
        /// Returns an error when the credential cannot be decoded into a complete,
        /// canonical protocol identity.
        fn credential_fingerprint(
            &self,
            challenge: &PaymentChallenge,
            credential: &PaymentCredential,
        ) -> Result<String>;

        async fn settle_payment(
            &self,
            challenge: &PaymentChallenge,
            credential: &PaymentCredential,
        ) -> Result<ProtocolSettlement>;
    }

    /// Shared bounded JSON and HTTP carrier rules used by payment protocol adapters.
    pub struct ProtocolWire;
    impl ProtocolWire {
        #[must_use]
        pub fn header_value<'a>(headers: &'a BTreeMap<String, String>, wanted: &str) -> Option<&'a str> {
            headers
                .iter()
                .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
                .map(|(_, value)| value.as_str())
        }
        pub fn canonical_json(value: &Value) -> Value {
            match value {
                Value::Array(items) => Value::Array(items.iter().map(ProtocolWire::canonical_json).collect()),
                Value::Object(fields) => {
                    let mut keys = fields.keys().collect::<Vec<_>>();
                    keys.sort_unstable();
                    let mut canonical = serde_json::Map::new();
                    for key in keys {
                        canonical.insert(key.clone(), ProtocolWire::canonical_json(&fields[key]));
                    }
                    Value::Object(canonical)
                }
                scalar => scalar.clone(),
            }
        }
        #[must_use]
        pub fn hash_values(values: &[&[u8]]) -> String {
            let mut hash = Sha256::new();
            for value in values {
                hash.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
                hash.update(value);
            }
            hex::encode(hash.finalize())
        }
        ///
        /// # Errors
        /// Returns a protocol error for an unsafe or non-HTTP resource URL.
        pub fn validate_resource(resource: &str) -> Result<()> {
            let parsed = reqwest::Url::parse(resource)
                .map_err(|_| Error::PaymentProtocol("payment resource is not an absolute URL".to_owned()))?;
            if !matches!(parsed.scheme(), "http" | "https")
                || parsed.host_str().is_none()
                || !parsed.username().is_empty()
                || parsed.password().is_some()
                || parsed.fragment().is_some()
            {
                return Err(Error::PaymentProtocol("payment resource URL is unsafe".to_owned()));
            }
            Ok(())
        }
        ///
        /// # Errors
        /// Returns a protocol error when the challenge belongs to another payment protocol.
        pub fn ensure_protocol(challenge: &PaymentChallenge, expected: &str) -> Result<()> {
            if challenge.protocol == expected {
                Ok(())
            } else {
                Err(Error::PaymentProtocol("protocol adapter mismatch".to_owned()))
            }
        }
        ///
        /// # Errors
        /// Returns a protocol error for missing or unknown wire fields.
        pub fn exact_object_fields(
            value: &Value,
            protocol: &str,
            required: &[&str],
            allowed: &[&str],
            name: &str,
        ) -> Result<()> {
            let object = value
                .as_object()
                .ok_or_else(|| Error::PaymentProtocol(format!("x402 {name} must be an object")))?;
            if required.iter().any(|field| !object.contains_key(*field))
                || object.keys().any(|field| !allowed.contains(&field.as_str()))
            {
                return Err(Error::PaymentProtocol(format!(
                    "{protocol} {name} fields do not match the official wire"
                )));
            }
            Ok(())
        }
        ///
        /// # Errors
        /// Returns a settlement error for invalid JSON, a missing discriminant or unexpected variant fields.
        pub fn strict_variant_wire(
            response: &[u8],
            protocol: &str,
            discriminant: &str,
            success_required: &[&str],
            success_allowed: &[&str],
            error_required: &[&str],
            error_allowed: &[&str],
        ) -> Result<Value> {
            let value: Value = serde_json::from_slice(response)
                .map_err(|error| Error::Settlement(format!("invalid {protocol} facilitator response: {error}")))?;
            let object = value
                .as_object()
                .ok_or_else(|| Error::Settlement(format!("{protocol} facilitator response must be an object")))?;
            let success = object.get(discriminant).and_then(Value::as_bool).ok_or_else(|| {
                Error::Settlement(format!("{protocol} facilitator response has no boolean {discriminant}"))
            })?;
            let (required, allowed) = if success {
                (success_required, success_allowed)
            } else {
                (error_required, error_allowed)
            };
            if required.iter().any(|field| !object.contains_key(*field))
                || object.keys().any(|field| !allowed.contains(&field.as_str()))
            {
                return Err(Error::Settlement(format!(
                    "{protocol} facilitator response fields do not match the official wire variant"
                )));
            }
            Ok(value)
        }
    }
    use sha2::{Digest as _, Sha256};
    #[cfg(test)]
    mod tests {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/contract_test.rs"));
    }
}
pub use agentvault_common::model::ProtocolSigningRequest;
pub use contract::*;

use model::{HttpCredentialDto, PaymentCredential, PaymentCredentialDto};
/// Adapter-owned projection of an intent-bound credential onto its HTTP wire form.
pub trait PaymentCredentialProjection: Sized {
    /// # Errors
    /// Rejects credentials whose protocol does not match the selected adapter.
    fn for_http(credential: PaymentCredential, origin: &str, protocol: &dyn crate::PaymentProtocol) -> Result<Self>;
}
impl PaymentCredentialProjection for PaymentCredentialDto {
    ///
    /// # Errors
    /// Returns a protocol error when the credential cannot be bound to the HTTP origin.
    fn for_http(credential: PaymentCredential, origin: &str, protocol: &dyn crate::PaymentProtocol) -> Result<Self> {
        if credential.protocol != protocol.name() {
            return Err(Error::PaymentProtocol(
                "HTTP credential adapter does not match its protocol".to_owned(),
            ));
        }
        let http = protocol.encode_http_credential(&credential)?;
        Ok(Self {
            protocol: credential.protocol,
            challenge_hash: credential.challenge_hash,
            credential_hash: credential.credential_hash,
            payload: credential.payload,
            http: Some(HttpCredentialDto {
                bound_origin: origin.to_owned(),
                header_name: http.header_name,
                header_value: http.header_value,
            }),
        })
    }
}
