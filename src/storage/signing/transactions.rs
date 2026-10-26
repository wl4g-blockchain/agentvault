//! Shared signing SQL transactions; concrete providers supply only driver types and locks.

macro_rules! signing_repository {
($repository:ident, $update:literal, $share:literal, $skip:literal, $intent_lock:literal, $client_lock:literal, $policy_lock:literal, $principal_lock:literal) => {
use crate::model::PaymentChallenge;
use crate::model::PaymentDirection;
use crate::model::PaymentStatus;
use crate::storage::payment::ReservationMutation;
use crate::storage::signing::SigningStorageRules;
use crate::storage::IdempotentWrite;
use crate::storage::OutboundSigningAuthorization;
use crate::storage::OutboundSigningIdentity;
use crate::storage::OutboundSigningRead;
use crate::storage::PersistedSignedPayment;
use crate::storage::SigningGuard;
use crate::storage::SigningStorage;
use crate::storage::SigningWrite;
use crate::utils::secure_material::SealedSensitiveMaterial;
use crate::Error;
use crate::Result;
use async_trait::async_trait;
use sqlx::Row as _;

#[async_trait]
impl SigningStorage for $repository {
    async fn load_for_principal(
        &self,
        expected_principal: Option<&str>,
        intent_id: &str,
    ) -> Result<Option<OutboundSigningRead>> {
        if expected_principal.is_some_and(str::is_empty) {
            return Err(Error::Unauthenticated);
        }
        let mut connection = self.base.pool().acquire().await.map_err(|error| {
            BaseRepository::storage_error("acquire outbound signing read connection", error)
        })?;
        let Some(identity) =
            Self::load_identity_for_access(&mut connection, expected_principal, intent_id).await?
        else {
            return Ok(None);
        };
        match identity.intent.status {
            PaymentStatus::Signed
            | PaymentStatus::Submitted
            | PaymentStatus::PendingSettlement
            | PaymentStatus::Unknown
            | PaymentStatus::Settled => {
                Self::load_signed(&mut connection, identity).await.map(Some)
            }
            PaymentStatus::Authorized => Self::load_authorized(&mut connection, identity)
                .await
                .map(Some),
            _ => Ok(None),
        }
    }

    async fn commit_signed(
        &self,
        write: SigningWrite<'_>,
    ) -> Result<IdempotentWrite<PersistedSignedPayment>> {
        let intent = write.intent.clone();
        let challenge = write.challenge.clone();
        let credential_hash = write.credential_hash.to_owned();
        let sealed_credential = write.sealed_credential.clone();
        let guard = write.guard.clone();
        let events = write.events.to_vec();
        let outbox = write.outbox.to_vec();
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    Self::lock_signing_principal(connection, &guard).await?;
                    let identity = Self::load_identity(connection, &guard.principal, &intent.id, false)
                        .await?
                        .ok_or(Error::Forbidden)?;
                    if Self::is_signed_replay_status(identity.intent.status) {
                        let locked = Self::load_identity(connection, &guard.principal, &intent.id, true)
                            .await?
                            .ok_or(Error::Forbidden)?;
                        return Self::replay_signed(connection, locked, &intent, &challenge).await;
                    }

                    Self::lock_signing_payment_authority(connection, &guard).await?;
                    let locked = Self::load_identity(connection, &guard.principal, &intent.id, true)
                        .await?
                        .ok_or(Error::Forbidden)?;
                    if Self::is_signed_replay_status(locked.intent.status) {
                        return Self::replay_signed(connection, locked, &intent, &challenge).await;
                    }
                    let authorization = Self::load_and_validate_guard(connection, &guard, &intent, &challenge).await?;
                    SigningStorageRules::validate_signing_events(
                        &events,
                        &intent.id,
                        &credential_hash,
                        &sealed_credential,
                    )?;
                    PaymentRepository::update_intent(
                        connection,
                        &intent,
                        guard.expected_intent_updated_at,
                        ReservationMutation::Unchanged,
                    )
                    .await?;
                    let result = sqlx::query(
                        "UPDATE pay_intent SET credential_hash = $1 WHERE id = $2 AND credential_hash IS NULL",
                    )
                    .bind(&credential_hash)
                    .bind(&intent.id)
                    .execute(&mut *connection)
                    .await
                    .map_err(|error| crate::Error::conflict(format!("signed credential binding rejected: {error}")))?;
                    if result.rows_affected() != 1 {
                        return Err(crate::Error::conflict("payment credential was already bound"));
                    }
                    PaymentRepository::append_side_effects(connection, &events, &outbox).await?;
                    Ok(IdempotentWrite::Inserted(PersistedSignedPayment {
                        identity: OutboundSigningIdentity {
                            client: authorization.identity.client,
                            scenario: authorization.identity.scenario,
                            intent,
                            challenge,
                        },
                        credential_hash,
                        sealed_credential,
                    }))
                })
            })
            .await
    }
}
impl $repository {
    async fn load_identity_for_access(
        connection: &mut Connection,
        expected_principal: Option<&str>,
        intent_id: &str,
    ) -> Result<Option<OutboundSigningIdentity>> {
        let principal: Option<String> = sqlx::query_scalar(
            "SELECT c.auth_principal FROM pay_intent i \
     JOIN pay_scenario s ON s.id = i.scenario_id \
     JOIN pay_client c ON c.id = s.client_id \
     WHERE i.id = $1 AND (CAST($2 AS TEXT) IS NULL OR c.auth_principal = $2) \
       AND c.enabled = TRUE AND s.enabled = TRUE",
        )
        .bind(intent_id)
        .bind(expected_principal)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| BaseRepository::storage_error("load accessible signing intent", error))?;
        let Some(principal) = principal else {
            return Ok(None);
        };
        Self::load_identity(connection, &principal, intent_id, false).await
    }

    const fn is_signed_replay_status(status: PaymentStatus) -> bool {
        matches!(
            status,
            PaymentStatus::Signed
                | PaymentStatus::Submitted
                | PaymentStatus::PendingSettlement
                | PaymentStatus::Unknown
                | PaymentStatus::Settled
        )
    }

    async fn replay_signed(
        connection: &mut Connection,
        identity: OutboundSigningIdentity,
        intended: &crate::model::PaymentIntent,
        challenge: &PaymentChallenge,
    ) -> Result<IdempotentWrite<PersistedSignedPayment>> {
        let OutboundSigningRead::Signed(existing) = Self::load_signed(connection, identity).await?
        else {
            return Err(Error::Storage(
                "signed payment replay has no credential".to_owned(),
            ));
        };
        if existing.identity.challenge != *challenge
            || !existing
                .identity
                .intent
                .same_immutable_identity_as(intended)
        {
            return Err(crate::Error::conflict(
                "signed payment replay differs from the committed credential",
            ));
        }
        Ok(IdempotentWrite::Replay(*existing))
    }

    async fn lock_signing_principal(
        connection: &mut Connection,
        guard: &SigningGuard,
    ) -> Result<()> {
        let scenario: Option<String> = sqlx::query_scalar(concat!(
            "SELECT s.id FROM pay_client c JOIN pay_scenario s ON s.client_id = c.id \
     WHERE c.id = $1 AND c.auth_principal = $2 AND s.id = $3 \
    ",
            $principal_lock
        ))
        .bind(&guard.client_id)
        .bind(&guard.principal)
        .bind(&guard.scenario_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| {
            BaseRepository::storage_error("lock outbound signing client and scenario", error)
        })?;
        if scenario.is_none() {
            return Err(Error::Forbidden);
        }
        Ok(())
    }

    async fn lock_signing_payment_authority(
        connection: &mut Connection,
        guard: &SigningGuard,
    ) -> Result<()> {
        let policy: Option<String> = sqlx::query_scalar(concat!(
            "SELECT id FROM pay_policy WHERE id = $1",
            $share
        ))
        .bind(&guard.policy_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| BaseRepository::storage_error("lock outbound signing policy", error))?;
        if policy.is_none() {
            return Err(crate::Error::conflict(
                "outbound signing policy authority changed",
            ));
        }
        let wallet: Option<String> = sqlx::query_scalar(concat!(
            "SELECT id FROM pay_wallet WHERE id = $1",
            $share
        ))
        .bind(&guard.wallet_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| BaseRepository::storage_error("lock outbound signing wallet", error))?;
        if wallet.is_none() {
            return Err(crate::Error::conflict(
                "outbound signing wallet authority changed",
            ));
        }
        let key: Option<i64> = sqlx::query_scalar(concat!(
            "SELECT version FROM wallet_key_version WHERE wallet_id = $1 AND version = $2 \
     AND status = 'active'",
            $share
        ))
        .bind(&guard.wallet_id)
        .bind(i64::from(guard.expected_key_version))
        .fetch_optional(connection)
        .await
        .map_err(|error| {
            BaseRepository::storage_error("lock outbound signing key version", error)
        })?;
        if key.is_none() {
            return Err(crate::Error::conflict(
                "outbound signing key authority changed",
            ));
        }
        Ok(())
    }

    async fn load_identity(
        connection: &mut Connection,
        principal: &str,
        intent_id: &str,
        lock_intent: bool,
    ) -> Result<Option<OutboundSigningIdentity>> {
        let query = if lock_intent {
            concat!(
                "SELECT i.*, s.id AS joined_scenario_id, s.client_id AS joined_client_id, \
     s.name AS scenario_name, s.enabled AS scenario_enabled, s.created_at AS scenario_created_at, \
     s.updated_at AS scenario_updated_at, c.id AS joined_client_row_id, c.name AS client_name, \
     c.auth_principal, c.enabled AS client_enabled, c.created_at AS client_created_at, \
     c.updated_at AS client_updated_at FROM pay_intent i \
     JOIN pay_scenario s ON s.id = i.scenario_id JOIN pay_client c ON c.id = s.client_id \
     WHERE i.id = $1 AND c.auth_principal = $2 AND c.enabled = TRUE AND s.enabled = TRUE",
                $intent_lock
            )
        } else {
            "SELECT i.*, s.id AS joined_scenario_id, s.client_id AS joined_client_id, \
     s.name AS scenario_name, s.enabled AS scenario_enabled, s.created_at AS scenario_created_at, \
     s.updated_at AS scenario_updated_at, c.id AS joined_client_row_id, c.name AS client_name, \
     c.auth_principal, c.enabled AS client_enabled, c.created_at AS client_created_at, \
     c.updated_at AS client_updated_at FROM pay_intent i \
     JOIN pay_scenario s ON s.id = i.scenario_id JOIN pay_client c ON c.id = s.client_id \
     WHERE i.id = $1 AND c.auth_principal = $2 AND c.enabled = TRUE AND s.enabled = TRUE"
        };
        let row = sqlx::query(query)
            .bind(intent_id)
            .bind(principal)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| {
                BaseRepository::storage_error("load principal-owned signing intent", error)
            })?;
        let Some(row) = row else { return Ok(None) };
        let intent = PaymentRepository::decode_row(&row)?;
        if intent.direction != PaymentDirection::Outbound {
            return Ok(None);
        }
        let client = crate::model::PaymentClient {
            id: row
                .try_get("joined_client_row_id")
                .map_err(Self::decode_join)?,
            name: row.try_get("client_name").map_err(Self::decode_join)?,
            auth_principal: row.try_get("auth_principal").map_err(Self::decode_join)?,
            enabled: row.try_get("client_enabled").map_err(Self::decode_join)?,
            created_at: row
                .try_get("client_created_at")
                .map_err(Self::decode_join)?,
            updated_at: row
                .try_get("client_updated_at")
                .map_err(Self::decode_join)?,
        };
        let scenario = crate::model::PaymentScenario {
            id: row
                .try_get("joined_scenario_id")
                .map_err(Self::decode_join)?,
            client_id: row.try_get("joined_client_id").map_err(Self::decode_join)?,
            name: row.try_get("scenario_name").map_err(Self::decode_join)?,
            enabled: row.try_get("scenario_enabled").map_err(Self::decode_join)?,
            created_at: row
                .try_get("scenario_created_at")
                .map_err(Self::decode_join)?,
            updated_at: row
                .try_get("scenario_updated_at")
                .map_err(Self::decode_join)?,
        };
        let challenge = Self::load_challenge(connection, &intent.id).await?;
        Ok(Some(OutboundSigningIdentity {
            client,
            scenario,
            intent,
            challenge,
        }))
    }

    fn decode_join(error: sqlx::Error) -> Error {
        BaseRepository::storage_error("decode outbound signing join", error)
    }

    async fn load_challenge(
        connection: &mut Connection,
        intent_id: &str,
    ) -> Result<PaymentChallenge> {
        let payload: serde_json::Value = sqlx::query_scalar(
    "SELECT payload FROM pay_event WHERE intent_id = $1 AND event_type = 'challenge_received' AND attempt_no = 0",
)
.bind(intent_id)
.fetch_optional(connection)
.await
.map_err(|error| BaseRepository::storage_error("load durable outbound challenge", error))?
.ok_or_else(|| Error::Storage("authorized outbound intent has no durable challenge".to_owned()))?;
        serde_json::from_value(
            payload.get("challenge").cloned().ok_or_else(|| {
                Error::Storage("challenge event payload is incomplete".to_owned())
            })?,
        )
        .map_err(|error| Error::Storage(format!("decode outbound payment challenge: {error}")))
    }

    async fn load_signed(
        connection: &mut Connection,
        identity: OutboundSigningIdentity,
    ) -> Result<OutboundSigningRead> {
        let row = sqlx::query(
            "SELECT e.signature_ref, e.payload, i.credential_hash AS intent_credential_hash \
     FROM pay_event e JOIN pay_intent i ON i.id = e.intent_id \
     WHERE e.intent_id = $1 AND e.event_type = 'signed' AND e.attempt_no = 0",
        )
        .bind(&identity.intent.id)
        .fetch_optional(connection)
        .await
        .map_err(|error| BaseRepository::storage_error("load durable signed credential", error))?
        .ok_or_else(|| Error::Storage("signed payment has no durable signed event".to_owned()))?;
        let credential_hash: String = row.try_get("signature_ref").map_err(Self::decode_join)?;
        let intent_credential_hash: Option<String> = row
            .try_get("intent_credential_hash")
            .map_err(Self::decode_join)?;
        let payload: serde_json::Value = row.try_get("payload").map_err(Self::decode_join)?;
        let sealed_credential: SealedSensitiveMaterial =
            serde_json::from_value(payload.get("sealed_credential").cloned().ok_or_else(|| {
                Error::Storage("signed event has no sealed credential".to_owned())
            })?)
            .map_err(|error| Error::Storage(format!("decode sealed signed credential: {error}")))?;
        if intent_credential_hash.as_deref() != Some(credential_hash.as_str())
            || payload
                .get("credential_hash")
                .and_then(serde_json::Value::as_str)
                != Some(credential_hash.as_str())
        {
            return Err(Error::Storage(
                "signed event credential fingerprint is inconsistent".to_owned(),
            ));
        }
        Ok(OutboundSigningRead::Signed(Box::new(
            PersistedSignedPayment {
                identity,
                credential_hash,
                sealed_credential,
            },
        )))
    }

    async fn load_authorized(
        connection: &mut Connection,
        identity: OutboundSigningIdentity,
    ) -> Result<OutboundSigningRead> {
        let policy_id =
            identity.intent.policy_id.as_deref().ok_or_else(|| {
                Error::Storage("authorized signing intent has no policy".to_owned())
            })?;
        let wallet_id =
            identity.intent.wallet_id.as_deref().ok_or_else(|| {
                Error::Storage("authorized signing intent has no wallet".to_owned())
            })?;
        let policy = PolicyRepository::load_policy_connection(connection, policy_id)
            .await?
            .ok_or_else(|| Error::Storage("authorized signing policy does not exist".to_owned()))?;
        let wallet = WalletRepository::load_payment_wallet_connection(connection, wallet_id)
            .await?
            .ok_or_else(|| Error::Storage("authorized signing wallet does not exist".to_owned()))?;
        let key_row = sqlx::query(
            "SELECT * FROM wallet_key_version WHERE wallet_id = $1 AND status = 'active'",
        )
        .bind(wallet_id)
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| BaseRepository::storage_error("load authorized signing key", error))?
        .ok_or_else(|| Error::Storage("authorized signing wallet has no active key".to_owned()))?;
        let active_key = WalletRepository::decode_key_row(&key_row)?;
        SigningStorageRules::validate_authorization_state(
            &identity,
            &policy,
            &wallet,
            &active_key,
            None,
        )?;
        Ok(OutboundSigningRead::Authorized(Box::new(
            OutboundSigningAuthorization {
                identity,
                policy,
                wallet,
                active_key,
            },
        )))
    }

    async fn load_and_validate_guard(
        connection: &mut Connection,
        guard: &SigningGuard,
        intent: &crate::model::PaymentIntent,
        challenge: &PaymentChallenge,
    ) -> Result<OutboundSigningAuthorization> {
        let identity = Self::load_identity(connection, &guard.principal, &intent.id, true).await?;
        let identity = identity.ok_or(Error::Forbidden)?;
        let OutboundSigningRead::Authorized(authorization) =
            Self::load_authorized(connection, identity).await?
        else {
            return Err(crate::Error::conflict(
                "payment is no longer authorized for signing",
            ));
        };
        let authorization = *authorization;
        if authorization.identity.client.id != guard.client_id
            || authorization.identity.client.updated_at != guard.expected_client_updated_at
            || authorization.identity.scenario.id != guard.scenario_id
            || authorization.identity.scenario.updated_at != guard.expected_scenario_updated_at
            || authorization.policy.id != guard.policy_id
            || authorization.policy.updated_at != guard.expected_policy_updated_at
            || authorization.wallet.wallet.id != guard.wallet_id
            || authorization.wallet.updated_at != guard.expected_wallet_updated_at
            || authorization.active_key.version != guard.expected_key_version
            || authorization.identity.intent.updated_at != guard.expected_intent_updated_at
            || authorization.identity.challenge.challenge_hash != guard.challenge_hash
            || authorization.identity.challenge != *challenge
            || !authorization
                .identity
                .intent
                .same_immutable_identity_as(intent)
            || intent.status != PaymentStatus::Signed
            || intent.updated_at <= guard.expected_intent_updated_at
        {
            return Err(crate::Error::conflict(
                "outbound signing authority changed before commit",
            ));
        }
        SigningStorageRules::validate_authorization_state(
            &authorization.identity,
            &authorization.policy,
            &authorization.wallet,
            &authorization.active_key,
            Some(guard.evaluated_at),
        )?;
        Ok(authorization)
    }
}



};
}
pub(crate) use signing_repository;

#[cfg(test)]
#[path = "../../../tests/storage/signing/transactions_test.rs"]
mod tests;
