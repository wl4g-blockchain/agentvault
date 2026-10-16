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

/// Bidirectional protocol adapter. It translates protocol wire structures into
/// the shared payment domain and never owns policy, keys, or financial state.
#[async_trait]
pub trait PaymentProtocol: Send + Sync {
    fn name(&self) -> &'static str;

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
