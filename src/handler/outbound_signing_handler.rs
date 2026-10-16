use std::sync::Arc;

use serde_json::json;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::{
    model::{
        canonicalize_payment_recipient, PaymentChallenge, PaymentCredential, PaymentDirection, PaymentEvent,
        PaymentEventType, PaymentIntent, PaymentStatus, ReservationStatus,
    },
    storage::{
        IdempotentWrite, IntentBoundPaymentSignatureRequest, OutboundSigningAuthorization, OutboundSigningIdentity,
        OutboundSigningRead, OutboxMessage, OutboxStatus, PersistedSignedPayment, SigningGuard, SigningStorage,
        SigningWrite, WalletCustody, WalletKeyStatus, WalletPaymentSigner,
    },
    utils::{
        aead::{parse_uuid_nonce, WALLET_KEY_ENCRYPTION_SCHEME},
        secure_material::SensitiveMaterialProtector,
    },
    Error, Result,
};

use super::{
    common::{canonical_http_origin, outbound_credential_context, validate_identifier},
    ProtocolRegistry, WalletBalanceSource, UNRESTRICTED_PRINCIPAL,
};

/// Authenticated request to sign one already-authorized outbound intent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignAuthorizedPayment {
    pub intent_id: String,
    pub challenge: PaymentChallenge,
}

/// Credential returned only after its encrypted recovery copy and the Signed
/// state have committed atomically.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SignedOutboundPayment {
    pub intent: PaymentIntent,
    pub credential: PaymentCredential,
    pub replayed: bool,
}

/// Collaborators for the intent-bound outbound signing application service.
pub struct OutboundSigningDependencies {
    pub storage: Arc<dyn SigningStorage>,
    pub protocols: Arc<ProtocolRegistry>,
    pub balances: Arc<dyn WalletBalanceSource>,
    pub signer: Arc<dyn WalletPaymentSigner>,
    pub material_protector: Arc<dyn SensitiveMaterialProtector>,
}

/// Revalidates authorization and balances immediately before an adapter-owned
/// payment digest reaches local custody.
pub struct OutboundSigningHandler {
    storage: Arc<dyn SigningStorage>,
    protocols: Arc<ProtocolRegistry>,
    balances: Arc<dyn WalletBalanceSource>,
    signer: Arc<dyn WalletPaymentSigner>,
    material_protector: Arc<dyn SensitiveMaterialProtector>,
}

impl OutboundSigningHandler {
    #[must_use]
    pub fn new(dependencies: OutboundSigningDependencies) -> Self {
        Self {
            storage: dependencies.storage,
            protocols: dependencies.protocols,
            balances: dependencies.balances,
            signer: dependencies.signer,
            material_protector: dependencies.material_protector,
        }
    }

    /// Signs one protocol payment and durably protects its credential.
    ///
    /// # Errors
    ///
    /// Fails closed for an unauthenticated principal, stale or mismatched
    /// Client/Scenario/Policy/Wallet/key bindings, an expired or non-authorized
    /// intent, insufficient token/native balance, protocol/signer mismatch, or
    /// an atomic persistence conflict.
    pub async fn sign(
        &self,
        principal: &str,
        command: &SignAuthorizedPayment,
        now: i64,
    ) -> Result<SignedOutboundPayment> {
        validate_request(principal, command)?;
        let expected_principal = (principal != UNRESTRICTED_PRINCIPAL).then_some(principal);
        let read = self
            .storage
            .load_for_principal(expected_principal, &command.intent_id)
            .await?
            .ok_or_else(|| Error::NotFound("authorized outbound payment".to_owned()))?;

        match read {
            OutboundSigningRead::Signed(record) => {
                validate_common_identity(principal, command, &record.identity, now)?;
                validate_replay_status(&record.identity.intent)?;
                self.open_persisted(*record, true)
            }
            OutboundSigningRead::Authorized(authorization) => {
                validate_common_identity(principal, command, &authorization.identity, now)?;
                let effective_principal = if principal == UNRESTRICTED_PRINCIPAL {
                    authorization.identity.client.auth_principal.clone()
                } else {
                    principal.to_owned()
                };
                self.sign_authorized(&effective_principal, *authorization, now).await
            }
        }
    }

    async fn sign_authorized(
        &self,
        principal: &str,
        mut authorization: OutboundSigningAuthorization,
        now: i64,
    ) -> Result<SignedOutboundPayment> {
        authorization.wallet = authorization
            .wallet
            .canonicalized_for_payment_network(&authorization.identity.intent.asset.network)
            .map_err(|field| Error::Storage(format!("invalid signing wallet {field}")))?;
        validate_authorization(&authorization, now)?;

        let prepared = self.create_credential(&authorization, now).await?;
        self.persist_credential(principal, &authorization, prepared, now).await
    }

    async fn create_credential(
        &self,
        authorization: &OutboundSigningAuthorization,
        now: i64,
    ) -> Result<PreparedCredential> {
        let adapter = self.protocols.get(&authorization.identity.intent.protocol)?;
        adapter
            .validate_payment(&authorization.identity.challenge, &authorization.identity.intent)
            .await?;

        let balances = self
            .balances
            .load(&authorization.wallet.wallet, &authorization.identity.intent.asset)
            .await?;
        if balances.asset_available < authorization.identity.intent.amount {
            return Err(Error::PolicyDenied(
                "wallet token balance is insufficient for this payment".to_owned(),
            ));
        }
        if balances.native_available < balances.minimum_native_required {
            return Err(Error::PolicyDenied(
                "wallet native balance is below the configured gas threshold".to_owned(),
            ));
        }

        let protocol_request = adapter
            .prepare_signing(
                &authorization.identity.challenge,
                &authorization.wallet.wallet.address,
                now,
            )
            .await?;
        if protocol_request.digest.is_empty() {
            return Err(Error::PaymentProtocol(
                "protocol adapter produced an empty signing digest".to_owned(),
            ));
        }
        let signature = self
            .signer
            .sign_payment(IntentBoundPaymentSignatureRequest::new(
                &authorization.identity.intent,
                &authorization.identity.challenge,
                &authorization.wallet,
                &authorization.active_key,
                &protocol_request,
            ))
            .await?;
        validate_signature_binding(authorization, &signature)?;

        let digest_hash = Sha256::digest(&protocol_request.digest);
        let credential = adapter
            .complete_signing(&authorization.identity.challenge, protocol_request, &signature.bytes)
            .await?;
        validate_credential(&authorization.identity, &credential)?;
        let plaintext = Zeroizing::new(serde_json::to_vec(&credential)?);
        let sealed_credential = self.material_protector.seal(
            &outbound_credential_context(&authorization.identity.intent.id),
            &plaintext,
        )?;

        Ok(PreparedCredential {
            credential,
            sealed_credential,
            digest_hash: hex::encode(digest_hash),
        })
    }

    async fn persist_credential(
        &self,
        principal: &str,
        authorization: &OutboundSigningAuthorization,
        prepared: PreparedCredential,
        now: i64,
    ) -> Result<SignedOutboundPayment> {
        let expected_updated_at = authorization.identity.intent.updated_at;
        let mut signed_intent = authorization.identity.intent.clone();
        signed_intent.status = PaymentStatus::Signed;
        signed_intent.next_reconcile_at = Some(now.saturating_add(1));
        signed_intent.updated_at = next_timestamp(signed_intent.updated_at, now);
        let events = signing_events(
            &signed_intent,
            &prepared.credential,
            authorization,
            &prepared.sealed_credential,
            &prepared.digest_hash,
            now,
        );
        let outbox = vec![signed_message(authorization, &prepared.credential, now)];
        let policy_id = authorization
            .identity
            .intent
            .policy_id
            .as_deref()
            .ok_or_else(|| Error::Storage("authorized intent has no policy binding".to_owned()))?;
        let wallet_id = authorization
            .identity
            .intent
            .wallet_id
            .as_deref()
            .ok_or_else(|| Error::Storage("authorized intent has no wallet binding".to_owned()))?;
        let guard = SigningGuard {
            principal: principal.to_owned(),
            client_id: authorization.identity.client.id.clone(),
            expected_client_updated_at: authorization.identity.client.updated_at,
            scenario_id: authorization.identity.scenario.id.clone(),
            expected_scenario_updated_at: authorization.identity.scenario.updated_at,
            policy_id: policy_id.to_owned(),
            expected_policy_updated_at: authorization.policy.updated_at,
            wallet_id: wallet_id.to_owned(),
            expected_wallet_updated_at: authorization.wallet.updated_at,
            expected_key_version: authorization.active_key.version,
            expected_intent_updated_at: expected_updated_at,
            challenge_hash: authorization.identity.challenge.challenge_hash.clone(),
            evaluated_at: now,
        };
        let write = SigningWrite {
            intent: &signed_intent,
            challenge: &authorization.identity.challenge,
            credential_hash: &prepared.credential.credential_hash,
            sealed_credential: &prepared.sealed_credential,
            guard: &guard,
            events: &events,
            outbox: &outbox,
        };
        let (persisted, replayed) = match self.storage.commit_signed(write).await? {
            IdempotentWrite::Inserted(record) => (record, false),
            IdempotentWrite::Replay(record) => (record, true),
        };
        self.open_persisted(persisted, replayed)
    }

    fn open_persisted(&self, persisted: PersistedSignedPayment, replayed: bool) -> Result<SignedOutboundPayment> {
        let plaintext = self.material_protector.open(
            &outbound_credential_context(&persisted.identity.intent.id),
            &persisted.sealed_credential,
        )?;
        let credential: PaymentCredential = serde_json::from_slice(&plaintext)
            .map_err(|_| Error::Crypto("sealed payment credential is invalid".to_owned()))?;
        validate_credential(&persisted.identity, &credential)?;
        if credential.credential_hash != persisted.credential_hash {
            return Err(Error::Storage(
                "sealed credential hash does not match its signed event".to_owned(),
            ));
        }
        Ok(SignedOutboundPayment {
            intent: persisted.identity.intent,
            credential,
            replayed,
        })
    }
}

struct PreparedCredential {
    credential: PaymentCredential,
    sealed_credential: crate::utils::secure_material::SealedSensitiveMaterial,
    digest_hash: String,
}

fn validate_request(principal: &str, command: &SignAuthorizedPayment) -> Result<()> {
    if principal.trim().is_empty() {
        return Err(Error::Unauthenticated);
    }
    validate_identifier("intent_id", &command.intent_id)?;
    if command.challenge.challenge_hash.trim().is_empty() || command.challenge.nonce.trim().is_empty() {
        return Err(Error::InvalidRequest(
            "payment challenge identity is incomplete".to_owned(),
        ));
    }
    Ok(())
}

fn validate_common_identity(
    principal: &str,
    command: &SignAuthorizedPayment,
    identity: &OutboundSigningIdentity,
    now: i64,
) -> Result<()> {
    if (principal != UNRESTRICTED_PRINCIPAL && !identity.client.authenticates(principal))
        || !identity.scenario.enabled
        || !identity.scenario.belongs_to(&identity.client.id)
        || identity.intent.scenario_id != identity.scenario.id
    {
        return Err(Error::Forbidden);
    }
    if identity.intent.id != command.intent_id
        || identity.challenge != command.challenge
        || identity.challenge.scenario_id != identity.scenario.id
        || identity.challenge.offer_id.is_some()
    {
        return Err(Error::PaymentConflict(
            "payment challenge does not match its durable intent".to_owned(),
        ));
    }
    if identity.intent.direction != PaymentDirection::Outbound
        || identity.intent.protocol != identity.challenge.protocol
        || identity.intent.asset != identity.challenge.asset
        || identity.intent.recipient != identity.challenge.recipient
        || identity.intent.amount != identity.challenge.amount
        || identity.intent.resource != identity.challenge.resource
        || identity.intent.challenge_hash.as_deref() != Some(identity.challenge.challenge_hash.as_str())
        || identity.intent.expires_at != Some(identity.challenge.expires_at)
    {
        return Err(Error::Storage(
            "outbound intent and durable challenge binding is invalid".to_owned(),
        ));
    }
    identity
        .intent
        .validate()
        .map_err(|field| Error::Storage(format!("invalid signing intent {field}")))?;
    if now < 0 || now >= identity.challenge.expires_at {
        return Err(Error::InvalidTransition(
            "payment challenge expired before signing or replay".to_owned(),
        ));
    }
    let canonical = canonicalize_payment_recipient(&identity.intent.asset.network, &identity.intent.recipient)
        .map_err(|_| Error::Storage("payment recipient is invalid for its network".to_owned()))?;
    if canonical != identity.intent.recipient {
        return Err(Error::Storage(
            "payment recipient is not stored in canonical form".to_owned(),
        ));
    }
    Ok(())
}

fn validate_replay_status(intent: &PaymentIntent) -> Result<()> {
    if matches!(
        intent.status,
        PaymentStatus::Signed
            | PaymentStatus::Submitted
            | PaymentStatus::PendingSettlement
            | PaymentStatus::Settled
            | PaymentStatus::Unknown
    ) {
        Ok(())
    } else {
        Err(Error::InvalidTransition(
            "payment intent has no replayable signed credential".to_owned(),
        ))
    }
}

fn validate_authorization(authorization: &OutboundSigningAuthorization, now: i64) -> Result<()> {
    let identity = &authorization.identity;
    let intent = &identity.intent;
    if intent.status != PaymentStatus::Authorized
        || intent.reservation_status != ReservationStatus::Reserved
        || intent.reserved_amount != intent.amount
    {
        return Err(Error::InvalidTransition(
            "outbound payment must hold its complete budget reservation before signing".to_owned(),
        ));
    }
    let policy_id = intent
        .policy_id
        .as_deref()
        .ok_or_else(|| Error::Storage("authorized intent has no policy binding".to_owned()))?;
    let wallet_id = intent
        .wallet_id
        .as_deref()
        .ok_or_else(|| Error::Storage("authorized intent has no wallet binding".to_owned()))?;
    let origin = canonical_http_origin(&intent.resource)?;
    let limits = authorization
        .policy
        .limits_for(PaymentDirection::Outbound)
        .ok_or_else(|| Error::PolicyDenied("payment policy no longer allows outbound payments".to_owned()))?;
    if !authorization.policy.enabled
        || authorization.policy.id != policy_id
        || authorization.policy.scenario_id != identity.scenario.id
        || !authorization.policy.allows_wallet(wallet_id)
        || !authorization.policy.allows_protocol(&intent.protocol)
        || !authorization.policy.allows_asset(&intent.asset)
        || !authorization
            .policy
            .allows_payment_recipient(&intent.asset.network, &intent.recipient)
        || !authorization.policy.allows_origin(&origin)
        || !authorization.policy.budget_unit.matches(&intent.asset)
        || intent.amount > limits.max_per_payment
        || limits
            .hard_daily_limit()
            .is_none_or(|hard_limit| intent.amount > hard_limit)
    {
        return Err(Error::PolicyDenied(
            "payment no longer matches its policy authorization".to_owned(),
        ));
    }
    authorization
        .policy
        .validate()
        .map_err(|field| Error::Storage(format!("invalid signing policy {field}")))?;
    if authorization.wallet.wallet.id != wallet_id
        || !authorization.wallet.signing_ready()
        || !matches!(authorization.wallet.custody, WalletCustody::Local)
        || authorization.wallet.budget_unit.as_ref() != Some(&authorization.policy.budget_unit)
        || authorization
            .wallet
            .max_per_payment
            .is_some_and(|limit| intent.amount > limit)
        || authorization
            .wallet
            .max_per_day
            .is_some_and(|limit| intent.amount > limit)
    {
        return Err(Error::PolicyDenied(
            "payment wallet is not enabled for local signing".to_owned(),
        ));
    }
    authorization
        .wallet
        .validate_payment_metadata()
        .map_err(|field| Error::Storage(format!("invalid payment wallet {field}")))?;
    let key = &authorization.active_key;
    if key.wallet_id != wallet_id
        || authorization.wallet.active_key_version != Some(key.version)
        || key.version == 0
        || key.status != WalletKeyStatus::Active
        || key.retired_at.is_some()
        || key.activated_at > now
        || !key
            .algorithm
            .eq_ignore_ascii_case(&authorization.wallet.wallet.algorithm)
        || !authorization.wallet.wallet.address_matches(&key.address)
        || key.public_key != authorization.wallet.wallet.public_key
        || key.encryption_scheme != WALLET_KEY_ENCRYPTION_SCHEME
        || key.kek_id.trim().is_empty()
        || key.wrapped_data_key.is_empty()
        || parse_uuid_nonce(&key.wrap_nonce).is_err()
        || key.key_ciphertext.is_empty()
        || parse_uuid_nonce(&key.key_nonce).is_err()
        || key.aad.is_empty()
    {
        return Err(Error::PolicyDenied(
            "wallet has no matching active local key".to_owned(),
        ));
    }
    Ok(())
}

fn validate_signature_binding(
    authorization: &OutboundSigningAuthorization,
    signature: &crate::storage::WalletPaymentSignature,
) -> Result<()> {
    let wallet_id = authorization.identity.intent.wallet_id.as_deref();
    if wallet_id != Some(signature.wallet_id.as_str())
        || signature.key_version != authorization.active_key.version
        || !authorization.wallet.wallet.address_matches(&signature.signer_address)
        || signature.bytes.is_empty()
    {
        return Err(Error::Crypto(
            "wallet signer returned material for a different key binding".to_owned(),
        ));
    }
    Ok(())
}

fn validate_credential(identity: &OutboundSigningIdentity, credential: &PaymentCredential) -> Result<()> {
    if credential.protocol != identity.intent.protocol
        || credential.challenge_hash != identity.challenge.challenge_hash
        || credential.credential_hash.trim().is_empty()
        || credential.credential_hash.len() > 256
        || credential.credential_hash.chars().any(char::is_control)
    {
        return Err(Error::PaymentProtocol(
            "payment credential is not bound to the authorized intent".to_owned(),
        ));
    }
    Ok(())
}

fn signing_events(
    intent: &PaymentIntent,
    credential: &PaymentCredential,
    authorization: &OutboundSigningAuthorization,
    sealed_credential: &crate::utils::secure_material::SealedSensitiveMaterial,
    digest_hash: &str,
    now: i64,
) -> Vec<PaymentEvent> {
    vec![
        PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::SignRequested,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload: json!({
                "protocol": intent.protocol,
                "wallet_id": authorization.wallet.wallet.id,
                "key_version": authorization.active_key.version,
                "digest_hash": digest_hash,
            }),
            created_at: now,
        },
        PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::Signed,
            attempt_no: 0,
            signature_ref: Some(credential.credential_hash.clone()),
            tx_hash: None,
            payload: json!({
                "credential_hash": credential.credential_hash,
                "sealed_credential": sealed_credential,
            }),
            created_at: now,
        },
    ]
}

fn signed_message(
    authorization: &OutboundSigningAuthorization,
    credential: &PaymentCredential,
    now: i64,
) -> OutboxMessage {
    let intent = &authorization.identity.intent;
    OutboxMessage {
        id: Uuid::new_v4().to_string(),
        aggregate_type: "payment_intent".to_owned(),
        aggregate_id: intent.id.clone(),
        event_type: "payment_signed".to_owned(),
        destination: format!("client:{}.payments", authorization.identity.client.id),
        correlation_id: intent.id.clone(),
        idempotency_key: format!("payment-signed:{}", intent.id),
        payload: json!({
            "intent_id": intent.id,
            "scenario_id": intent.scenario_id,
            "credential_hash": credential.credential_hash,
        }),
        status: OutboxStatus::Pending,
        attempt_count: 0,
        available_at: now,
        lease_owner: None,
        lease_expires_at: None,
        last_error: None,
        created_at: now,
        delivered_at: None,
    }
}

fn next_timestamp(previous: i64, now: i64) -> i64 {
    now.max(previous.saturating_add(1))
}

#[cfg(test)]
#[path = "../../tests/handler/outbound_signing_handler_test.rs"]
mod tests;
