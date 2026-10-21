use std::collections::BTreeMap;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    model::{PaymentChallenge, PaymentCredential, PaymentIntent, PaymentOffer, PaymentReceipt},
    Error, Result,
};

/// Protocol-owned request for one intent-bound wallet signature.
///
/// The core signer accepts this adapter-produced digest only; no transport or
/// business caller can supply an arbitrary digest. `payload` is an unsigned
/// protocol template and must not be logged.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ProtocolSigningRequest {
    pub digest: Vec<u8>,
    pub payload: Value,
}

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

    async fn build_credential(&self, challenge: &PaymentChallenge, signed_payload: Value) -> Result<PaymentCredential>;

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
    async fn build_challenge(&self, offer: &PaymentOffer, recipient: &str, now: i64) -> Result<PaymentChallenge>;

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
    fn credential_fingerprint(&self, challenge: &PaymentChallenge, credential: &PaymentCredential) -> Result<String>;

    async fn settle_payment(
        &self,
        challenge: &PaymentChallenge,
        credential: &PaymentCredential,
    ) -> Result<ProtocolSettlement>;
}

#[cfg(test)]
#[path = "../../tests/protocol/adapter_test.rs"]
mod tests;
