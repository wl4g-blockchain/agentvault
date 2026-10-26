//! Signing persistence contract.

use crate::{
    model::{PaymentChallenge, PaymentClient, PaymentEvent, PaymentIntent, PaymentPolicy, PaymentScenario},
    protocol::ProtocolSigningRequest,
    storage::{EncryptedWalletKeyVersion, IdempotentWrite, OutboxMessage, PaymentWallet},
    utils::secure_material::SealedSensitiveMaterial,
    Result,
};
use async_trait::async_trait;

pub mod postgres;
pub mod sqlite;
pub(crate) mod transactions;

/// Principal-owned identity common to both a new signature and a durable
/// signed-payment replay.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundSigningIdentity {
    pub client: PaymentClient,
    pub scenario: PaymentScenario,
    pub intent: PaymentIntent,
    /// Complete public protocol challenge recovered from the immutable
    /// `ChallengeReceived` event. A hash alone is not sufficient to recreate
    /// an adapter-owned signing payload after a crash.
    pub challenge: PaymentChallenge,
}

/// Current authorization facts needed immediately before using a local key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboundSigningAuthorization {
    pub identity: OutboundSigningIdentity,
    pub policy: PaymentPolicy,
    pub wallet: PaymentWallet,
    pub active_key: EncryptedWalletKeyVersion,
}

/// AEAD-protected result of a completed outbound signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PersistedSignedPayment {
    pub identity: OutboundSigningIdentity,
    pub credential_hash: String,
    pub sealed_credential: SealedSensitiveMaterial,
}

/// Authoritative read result. A signed replay deliberately does not require
/// the old key to remain active: it returns the credential committed by the
/// original transaction and can never cause another signature.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OutboundSigningRead {
    Authorized(Box<OutboundSigningAuthorization>),
    Signed(Box<PersistedSignedPayment>),
}

/// Snapshots revalidated while the signing transaction holds all authority
/// rows. These values are optimistic-concurrency tokens, never authorization
/// assertions supplied by a caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SigningGuard {
    pub principal: String,
    pub client_id: String,
    pub expected_client_updated_at: i64,
    pub scenario_id: String,
    pub expected_scenario_updated_at: i64,
    pub policy_id: String,
    pub expected_policy_updated_at: i64,
    pub wallet_id: String,
    pub expected_wallet_updated_at: i64,
    pub expected_key_version: u32,
    pub expected_intent_updated_at: i64,
    pub challenge_hash: String,
    pub evaluated_at: i64,
}

/// One atomic `Authorized -> Signed` persistence operation.
pub struct SigningWrite<'a> {
    pub intent: &'a PaymentIntent,
    pub challenge: &'a PaymentChallenge,
    pub credential_hash: &'a str,
    pub sealed_credential: &'a SealedSensitiveMaterial,
    pub guard: &'a SigningGuard,
    pub events: &'a [PaymentEvent],
    pub outbox: &'a [OutboxMessage],
}

/// Intent-bound request accepted by a wallet payment signer.
///
/// The constructor is crate-private so HTTP, MQTT, and SDK transports cannot
/// turn this port into an arbitrary-digest signing oracle. Only the payment
/// application service can combine an authorized intent with an
/// adapter-produced [`ProtocolSigningRequest`].
pub struct IntentBoundPaymentSignatureRequest<'a> {
    intent: &'a PaymentIntent,
    challenge: &'a PaymentChallenge,
    wallet: &'a PaymentWallet,
    active_key: &'a EncryptedWalletKeyVersion,
    protocol_request: &'a ProtocolSigningRequest,
}

impl<'a> IntentBoundPaymentSignatureRequest<'a> {
    pub(crate) const fn new(
        intent: &'a PaymentIntent,
        challenge: &'a PaymentChallenge,
        wallet: &'a PaymentWallet,
        active_key: &'a EncryptedWalletKeyVersion,
        protocol_request: &'a ProtocolSigningRequest,
    ) -> Self {
        Self {
            intent,
            challenge,
            wallet,
            active_key,
            protocol_request,
        }
    }

    #[must_use]
    pub const fn intent(&self) -> &PaymentIntent {
        self.intent
    }

    #[must_use]
    pub const fn challenge(&self) -> &PaymentChallenge {
        self.challenge
    }

    #[must_use]
    pub const fn wallet(&self) -> &PaymentWallet {
        self.wallet
    }

    #[must_use]
    pub const fn active_key(&self) -> &EncryptedWalletKeyVersion {
        self.active_key
    }

    /// Adapter-produced digest. No public constructor accepts raw bytes.
    #[must_use]
    pub fn digest(&self) -> &[u8] {
        &self.protocol_request.digest
    }
}

/// Recoverable signature together with the key identity reported by the
/// custody adapter. The handler rejects a result for any other wallet/version.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletPaymentSignature {
    pub wallet_id: String,
    pub key_version: u32,
    pub signer_address: String,
    pub bytes: Vec<u8>,
}

/// Local-custody boundary used only for an already authorized payment.
/// Implementations unwrap the supplied encrypted key version inside their
/// custody boundary and must not expose a separate raw-digest transport API.
#[async_trait]
pub trait WalletPaymentSigner: Send + Sync {
    async fn sign_payment(&self, request: IntentBoundPaymentSignatureRequest<'_>) -> Result<WalletPaymentSignature>;
}

/// Atomic persistence boundary for outbound signing.
#[async_trait]
pub trait SigningStorage: Send + Sync {
    /// Loads an intent through `expected_principal -> enabled client -> owned
    /// scenario`. `None` is reserved for the explicitly configured open HTTP
    /// edge; the handler replaces it with the persisted client principal
    /// before any signing commit. `Authorized` reads recover the complete challenge and join
    /// the exact enabled outbound policy, explicit policy-wallet membership,
    /// enabled local-custody wallet, and active non-compromised key. `Signed`
    /// reads recover the immutable challenge plus the previously sealed
    /// credential and never require or expose private-key material.
    async fn load_for_principal(
        &self,
        expected_principal: Option<&str>,
        intent_id: &str,
    ) -> Result<Option<OutboundSigningRead>>;

    /// Atomically performs `Authorized -> Signed`, appends exactly one
    /// `SignRequested` and one `Signed` event, and inserts the outgoing
    /// notification. While holding the relevant rows, implementations must:
    ///
    /// - re-resolve the principal, enabled client, enabled owned scenario,
    ///   intent, enabled outbound policy, explicit policy-wallet membership,
    ///   enabled local wallet, and active non-compromised key;
    /// - require every revision/key version in [`SigningGuard`] to match;
    /// - require an unexpired outbound `Authorized` intent with a full
    ///   reservation equal to its amount and an immutable identity matching
    ///   `challenge` (including the complete event payload);
    /// - recheck protocol, network, asset, recipient, origin, and directional
    ///   policy scope before applying the compare-and-swap; and
    /// - store only the AEAD envelope, never credential or key plaintext.
    ///
    /// If a concurrent transaction already committed the same immutable
    /// intent and complete challenge, return `Replay` with the winning
    /// persisted envelope and discard the losing candidate. Adapter time
    /// fields may make two locally valid candidate credentials differ, but
    /// they must never replace the first committed credential. A differing
    /// challenge, authority guard, or signed identity is a conflict and must
    /// fail closed.
    async fn commit_signed(&self, write: SigningWrite<'_>) -> Result<IdempotentWrite<PersistedSignedPayment>>;
}

#[cfg(test)]
#[path = "../../../tests/storage/signing/mod_test.rs"]
mod tests;

mod validation {
    use crate::{
        model::{PaymentDirection, PaymentEventType, PaymentStatus, ReservationStatus},
        storage::{OutboundSigningIdentity, WalletCustody, WalletKeyStatus},
        utils::secure_material::SealedSensitiveMaterial,
        Error, Result,
    };

    pub(crate) struct SigningStorageRules;
    impl SigningStorageRules {
        pub(crate) fn canonical_origin(resource: &str) -> Result<String> {
            let parsed = reqwest::Url::parse(resource)
                .map_err(|_| Error::PolicyDenied("payment resource has no canonical HTTP origin".to_owned()))?;
            if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
                return Err(Error::PolicyDenied(
                    "payment resource has no canonical HTTP origin".to_owned(),
                ));
            }
            Ok(parsed.origin().ascii_serialization())
        }
        pub(crate) fn validate_authorization_state(
            identity: &OutboundSigningIdentity,
            policy: &crate::model::PaymentPolicy,
            wallet: &crate::storage::PaymentWallet,
            key: &crate::storage::EncryptedWalletKeyVersion,
            evaluated_at: Option<i64>,
        ) -> Result<()> {
            let intent = &identity.intent;
            let challenge = &identity.challenge;
            let origin = Self::canonical_origin(&intent.resource)?;
            let Some(limits) = policy.limits_for(PaymentDirection::Outbound) else {
                return Err(Error::PolicyDenied(
                    "outbound payment signing authority is no longer valid".to_owned(),
                ));
            };
            if intent.status != PaymentStatus::Authorized
                || intent.direction != PaymentDirection::Outbound
                || intent.reservation_status != ReservationStatus::Reserved
                || intent.reserved_amount != intent.amount
                || evaluated_at.is_some_and(|now| challenge.expires_at <= now)
                || intent.expires_at != Some(challenge.expires_at)
                || intent.challenge_hash.as_deref() != Some(challenge.challenge_hash.as_str())
                || challenge.scenario_id != intent.scenario_id
                || challenge.offer_id.is_some()
                || challenge.resource != intent.resource
                || challenge.protocol != intent.protocol
                || challenge.asset != intent.asset
                || challenge.amount != intent.amount
                || !crate::model::payment_recipients_match(
                    &intent.asset.network,
                    &challenge.recipient,
                    &intent.recipient,
                )
                || challenge.nonce.trim().is_empty()
                || identity.scenario.id != intent.scenario_id
                || identity.scenario.client_id != identity.client.id
                || intent.policy_id.as_deref() != Some(policy.id.as_str())
                || intent.wallet_id.as_deref() != Some(wallet.wallet.id.as_str())
                || !policy.enabled
                || policy.scenario_id != identity.scenario.id
                || !policy.allows_wallet(&wallet.wallet.id)
                || !policy.allows_direction(PaymentDirection::Outbound)
                || !policy.allows_protocol(&intent.protocol)
                || !policy.allows_asset(&intent.asset)
                || !policy.allows_payment_recipient(&intent.asset.network, &intent.recipient)
                || !policy.allows_origin(&origin)
                || !policy.budget_unit.matches(&intent.asset)
                || intent.amount > limits.max_per_payment
                || limits.hard_daily_limit().is_none_or(|maximum| intent.amount > maximum)
                || !wallet.signing_ready_for_payment_network(&intent.asset.network)
                || wallet.custody != WalletCustody::Local
                || wallet.budget_unit.as_ref() != Some(&intent.asset.budget_unit())
                || wallet.max_per_payment.is_some_and(|maximum| intent.amount > maximum)
                || wallet.max_per_day.is_some_and(|maximum| intent.amount > maximum)
                || wallet.active_key_version != Some(key.version)
                || key.status != WalletKeyStatus::Active
                || key.wallet_id != wallet.wallet.id
                || key.algorithm != wallet.wallet.algorithm
                || key.address != wallet.wallet.address
                || key.public_key != wallet.wallet.public_key
                || key.version == 0
                || evaluated_at.is_some_and(|now| key.activated_at > now)
                || key.encryption_scheme.trim().is_empty()
                || key.kek_id.trim().is_empty()
                || key.wrapped_data_key.is_empty()
                || key.wrap_nonce.is_empty()
                || key.key_ciphertext.is_empty()
                || key.key_nonce.is_empty()
                || key.aad.is_empty()
                || key.retired_at.is_some()
            {
                return Err(Error::PolicyDenied(
                    "outbound payment signing authority is no longer valid".to_owned(),
                ));
            }
            Ok(())
        }
        pub(crate) fn validate_signing_events(
            events: &[crate::model::PaymentEvent],
            intent_id: &str,
            credential_hash: &str,
            sealed_credential: &SealedSensitiveMaterial,
        ) -> Result<()> {
            let requested = events
                .iter()
                .filter(|event| event.event_type == PaymentEventType::SignRequested)
                .collect::<Vec<_>>();
            let signed = events
                .iter()
                .filter(|event| event.event_type == PaymentEventType::Signed)
                .collect::<Vec<_>>();
            if events.len() != 2
                || requested.len() != 1
                || signed.len() != 1
                || events
                    .iter()
                    .any(|event| event.intent_id != intent_id || event.attempt_no != 0)
                || signed[0].signature_ref.as_deref() != Some(credential_hash)
                || signed[0]
                    .payload
                    .get("credential_hash")
                    .and_then(serde_json::Value::as_str)
                    != Some(credential_hash)
                || signed[0]
                    .payload
                    .get("sealed_credential")
                    .cloned()
                    .and_then(|value| serde_json::from_value::<SealedSensitiveMaterial>(value).ok())
                    .as_ref()
                    != Some(sealed_credential)
            {
                return Err(crate::Error::conflict(
                    "outbound signing events are incomplete or inconsistent",
                ));
            }
            Ok(())
        }
    }
}
pub(crate) use validation::*;
