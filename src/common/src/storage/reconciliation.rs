use super::reconciliation_posting::ReconciliationPosting;
use crate::{
    model::PaymentDirection, model::PaymentEvent, model::PaymentEventType, model::PaymentIntent, model::PaymentStatus,
    model::ReservationStatus, storage::IIntentRepository, storage::OutboxMessage, storage::OutboxStatus,
    storage::PaymentProgressWrite, storage::ReconciliationLeaseRequest, storage::SettlementWrite,
    util::clock::SystemUnixClock, util::clock::UnixClock, Error, Result,
};
use serde_json::json;
use std::{sync::Arc, time::Duration};
use tracing::Instrument as _;

const MAX_BATCH_SIZE: u32 = 1_000;

/// Runtime limits for one reconciliation worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconciliationConfig {
    pub worker_id: String,
    pub batch_size: u32,
    pub lease_duration: Duration,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
}

impl ReconciliationConfig {
    /// Validates limits before a worker starts claiming durable work.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for an empty worker identifier, an
    /// invalid batch size, or a duration that cannot be represented in the
    /// persisted second-resolution timestamps.
    pub fn validate(&self) -> Result<()> {
        if self.worker_id.trim().is_empty() {
            return Err(Error::Config("reconciliation worker_id must not be empty".to_owned()));
        }
        if self.batch_size == 0 || self.batch_size > MAX_BATCH_SIZE {
            return Err(Error::Config(format!(
                "reconciliation batch_size must be between 1 and {MAX_BATCH_SIZE}"
            )));
        }
        PaymentReconciler::duration_seconds(self.lease_duration, "lease_duration")?;
        let initial = PaymentReconciler::duration_seconds(self.initial_backoff, "initial_backoff")?;
        let maximum = PaymentReconciler::duration_seconds(self.max_backoff, "max_backoff")?;
        if initial > maximum {
            return Err(Error::Config(
                "reconciliation initial_backoff must not exceed max_backoff".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Read-only settlement evidence boundary. Provider and chain-specific query
/// behavior belongs in adapters, not in the scheduling job.
pub struct PaymentReconciler {
    direction: Option<PaymentDirection>,
    storage: Arc<dyn IIntentRepository>,
    evidence: Arc<dyn ReconciliationEvidenceSource>,
    clock: Arc<dyn UnixClock>,
    config: ReconciliationConfig,
    lease_seconds: i64,
    initial_backoff_seconds: i64,
    max_backoff_seconds: i64,
}

impl PaymentReconciler {
    /// Builds a worker using the host wall clock.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker configuration is invalid.
    pub fn new(
        storage: Arc<dyn IIntentRepository>,
        evidence: Arc<dyn ReconciliationEvidenceSource>,
        config: ReconciliationConfig,
    ) -> Result<Self> {
        Self::with_clock(storage, evidence, Arc::new(SystemUnixClock), config)
    }

    /// Builds a worker with an injected clock.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker configuration is invalid.
    pub fn with_clock(
        storage: Arc<dyn IIntentRepository>,
        evidence: Arc<dyn ReconciliationEvidenceSource>,
        clock: Arc<dyn UnixClock>,
        config: ReconciliationConfig,
    ) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            direction: None,
            storage,
            evidence,
            clock,
            lease_seconds: PaymentReconciler::duration_seconds(config.lease_duration, "lease_duration")?,
            initial_backoff_seconds: PaymentReconciler::duration_seconds(config.initial_backoff, "initial_backoff")?,
            max_backoff_seconds: PaymentReconciler::duration_seconds(config.max_backoff, "max_backoff")?,
            config,
        })
    }

    /// Runs one bounded reconciliation pass.
    ///
    /// Provider/RPC lookup failures are rescheduled and reflected in the
    /// returned summary. Storage failures are returned because losing a lease,
    /// state transition, or ledger commit must fail closed.
    ///
    /// # Errors
    ///
    /// Returns a clock or authoritative storage error.
    pub async fn run_once(&self) -> Result<ReconciliationRun> {
        let claim_now = self.clock.now()?;
        let claim = ReconciliationLeaseRequest {
            direction: self.direction,
            owner: self.config.worker_id.clone(),
            now: claim_now,
            lease_until: PaymentReconciler::checked_timestamp_add(
                claim_now,
                self.lease_seconds,
                "reconciliation lease",
            )?,
            limit: self.config.batch_size,
        };
        let claims = self.storage.claim_reconciliation(&claim).await?;
        let mut run = ReconciliationRun {
            claimed: u32::try_from(claims.len()).unwrap_or(u32::MAX),
            ..ReconciliationRun::default()
        };
        for claim in claims {
            let span = tracing::info_span!(
                "payment.reconciliation.attempt",
                run.id = %claim.intent.run_id,
                payment.intent.id = %claim.intent.id,
                payment.attempt_no = claim.attempt_no,
            );
            async {
                let now = self.clock.now()?;
                self.storage
                    .renew_reconciliation_lease(
                        &claim.intent.id,
                        &self.config.worker_id,
                        PaymentReconciler::checked_timestamp_add(now, self.lease_seconds, "reconciliation lease")?,
                    )
                    .await?;
                self.reconcile_intent(&claim.intent, claim.attempt_no, now, &mut run)
                    .await
            }
            .instrument(span)
            .await?;
        }
        Ok(run)
    }

    async fn reconcile_intent(
        &self,
        intent: &PaymentIntent,
        attempt_no: u32,
        now: i64,
        run: &mut ReconciliationRun,
    ) -> Result<()> {
        if intent.status == PaymentStatus::Created && intent.expires_at.is_some_and(|expires_at| expires_at <= now) {
            self.commit_expired_challenge(intent, attempt_no, now).await?;
            run.expired = run.expired.saturating_add(1);
            return Ok(());
        }
        if !intent.needs_reconciliation() {
            run.skipped = run.skipped.saturating_add(1);
            return self.reschedule(intent, now).await;
        }
        match self.evidence.lookup(intent, now).await {
            Ok(ReconciliationEvidence::Confirmed(evidence)) => {
                match self.commit_confirmed(intent, evidence.as_ref(), attempt_no, now).await {
                    Ok(()) => run.settled = run.settled.saturating_add(1),
                    Err(Error::Settlement(reason)) => {
                        tracing::warn!(intent_id = %intent.id, %reason, "rejected untrusted reconciliation evidence");
                        run.unknown = run.unknown.saturating_add(1);
                        self.reschedule(intent, now).await?;
                    }
                    Err(error) => return Err(error),
                }
            }
            Ok(ReconciliationEvidence::Verified { verification_ref }) => {
                self.commit_verified(intent, &verification_ref, attempt_no, now).await?;
                run.verified = run.verified.saturating_add(1);
            }
            Ok(ReconciliationEvidence::Pending) => {
                run.pending = run.pending.saturating_add(1);
                self.reschedule(intent, now).await?;
            }
            Ok(ReconciliationEvidence::Expired { reason }) => {
                self.commit_expired_settlement(intent, attempt_no, now, &reason).await?;
                run.expired = run.expired.saturating_add(1);
            }
            Ok(ReconciliationEvidence::Unknown { reason }) => {
                tracing::warn!(intent_id = %intent.id, %reason, "settlement remains unknown");
                run.unknown = run.unknown.saturating_add(1);
                self.reschedule(intent, now).await?;
            }
            Ok(ReconciliationEvidence::Unavailable { reason }) => {
                tracing::warn!(intent_id = %intent.id, %reason, "settlement query is unavailable");
                run.unavailable = run.unavailable.saturating_add(1);
                self.reschedule(intent, now).await?;
            }
            Err(error) => {
                tracing::warn!(intent_id = %intent.id, error = %error, "settlement evidence lookup failed");
                run.provider_errors = run.provider_errors.saturating_add(1);
                self.reschedule(intent, now).await?;
            }
        }
        Ok(())
    }

    async fn commit_expired_challenge(&self, intent: &PaymentIntent, attempt_no: u32, now: i64) -> Result<()> {
        if intent.status != PaymentStatus::Created
            || intent.expires_at.is_none_or(|expires_at| expires_at > now)
            || intent.reservation_status != ReservationStatus::Reserved
        {
            return Err(Error::Settlement(
                "challenge expiry does not match a due reserved intent".to_owned(),
            ));
        }
        self.commit_expired(intent, attempt_no, now, "challenge_expiry", "challenge expired")
            .await
    }

    async fn commit_expired_settlement(
        &self,
        intent: &PaymentIntent,
        attempt_no: u32,
        now: i64,
        reason: &str,
    ) -> Result<()> {
        if intent.direction != PaymentDirection::Inbound
            || intent.protocol != "x402"
            || !matches!(intent.status, PaymentStatus::PendingSettlement | PaymentStatus::Unknown)
            || intent.reservation_status != ReservationStatus::Reserved
        {
            return Err(Error::Settlement(
                "authorization expiry does not match a recoverable inbound settlement".to_owned(),
            ));
        }
        self.commit_expired(intent, attempt_no, now, "settlement_authorization_expiry", reason)
            .await
    }

    async fn commit_expired(
        &self,
        intent: &PaymentIntent,
        attempt_no: u32,
        now: i64,
        stage: &str,
        reason: &str,
    ) -> Result<()> {
        let updated_at = now.max(intent.updated_at.saturating_add(1));
        let mut expired = intent.clone();
        expired.status = PaymentStatus::Expired;
        expired.reservation_status = ReservationStatus::Released;
        expired.next_reconcile_at = None;
        expired.updated_at = updated_at;
        let material = format!("{}:{stage}", intent.id);
        let event = PaymentEvent {
            id: ReconciliationPosting::stable_id("event", &material),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::Failed,
            attempt_no,
            signature_ref: None,
            tx_hash: None,
            payload: json!({"stage": stage, "reason": reason}),
            created_at: updated_at,
        };
        let outbox = OutboxMessage {
            id: ReconciliationPosting::stable_id("outbox", &material),
            aggregate_type: "payment_intent".to_owned(),
            aggregate_id: intent.id.clone(),
            event_type: "payment_expired".to_owned(),
            destination: format!("scenario:{}.payments", intent.scenario_id),
            run_id: intent.run_id.clone(),
            payload: json!({
                "intent_id": intent.id,
                "scenario_id": intent.scenario_id,
                "status": PaymentStatus::Expired.as_str(),
                "reason": stage,
            }),
            status: OutboxStatus::Pending,
            attempt_count: 0,
            available_at: updated_at,
            lease_owner: None,
            lease_expires_at: None,
            last_error: None,
            created_at: updated_at,
            delivered_at: None,
        };
        self.storage
            .commit_progress(PaymentProgressWrite {
                intent: &expired,
                expected_updated_at: intent.updated_at,
                events: std::slice::from_ref(&event),
                outbox: std::slice::from_ref(&outbox),
            })
            .await?;
        Ok(())
    }

    async fn commit_confirmed(
        &self,
        intent: &PaymentIntent,
        evidence: &ConfirmedSettlement,
        attempt_no: u32,
        now: i64,
    ) -> Result<()> {
        evidence.validate_for(intent, now)?;
        let commit = ReconciliationPosting::build_reconciliation_commit(intent, evidence, attempt_no, now)?;
        self.storage
            .commit_settlement(SettlementWrite {
                intent: &commit.intent,
                expected_updated_at: intent.updated_at,
                events: std::slice::from_ref(&commit.event),
                ledger_entries: std::slice::from_ref(&commit.ledger),
                outbox: std::slice::from_ref(&commit.outbox),
            })
            .await?;
        Ok(())
    }

    async fn commit_verified(
        &self,
        intent: &PaymentIntent,
        verification_ref: &str,
        attempt_no: u32,
        now: i64,
    ) -> Result<()> {
        if intent.status != PaymentStatus::Submitted || verification_ref.trim().is_empty() {
            return Err(Error::Settlement(
                "verified reconciliation evidence does not match a submitted intent".to_owned(),
            ));
        }
        let updated_at = now.max(intent.updated_at.saturating_add(1));
        let mut verified = intent.clone();
        verified.status = PaymentStatus::PendingSettlement;
        verified.updated_at = updated_at;
        verified.next_reconcile_at = Some(PaymentReconciler::checked_timestamp_add(
            updated_at,
            self.initial_backoff_seconds,
            "verified settlement retry",
        )?);
        let event = PaymentEvent {
            id: ReconciliationPosting::stable_id("event", &format!("{}:verified:{verification_ref}", intent.id)),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::CredentialVerified,
            attempt_no,
            signature_ref: None,
            tx_hash: None,
            payload: json!({"verification_ref": verification_ref, "recovered": true}),
            created_at: updated_at,
        };
        self.storage
            .commit_progress(PaymentProgressWrite {
                intent: &verified,
                expected_updated_at: intent.updated_at,
                events: std::slice::from_ref(&event),
                outbox: &[],
            })
            .await?;
        Ok(())
    }

    async fn reschedule(&self, intent: &PaymentIntent, now: i64) -> Result<()> {
        let delay = self.next_backoff(intent);
        self.storage
            .release_reconciliation_lease(
                &intent.id,
                &self.config.worker_id,
                PaymentReconciler::checked_timestamp_add(now, delay, "reconciliation retry")?,
            )
            .await
    }

    fn next_backoff(&self, intent: &PaymentIntent) -> i64 {
        let previous = intent
            .next_reconcile_at
            .and_then(|next| next.checked_sub(intent.updated_at))
            .filter(|delay| *delay >= self.initial_backoff_seconds)
            .unwrap_or(0);
        if previous == 0 {
            self.initial_backoff_seconds
        } else {
            previous.saturating_mul(2).min(self.max_backoff_seconds)
        }
    }
}

impl PaymentReconciler {
    pub(crate) fn duration_seconds(duration: Duration, name: &str) -> Result<i64> {
        let seconds = duration.as_secs();
        if duration.is_zero() || duration.subsec_nanos() != 0 || i64::try_from(seconds).is_err() {
            return Err(Error::Config(format!(
                "reconciliation {name} must be a positive whole number of seconds"
            )));
        }
        i64::try_from(seconds).map_err(|_| Error::Config(format!("reconciliation {name} exceeds timestamp range")))
    }

    pub(crate) fn checked_timestamp_add(timestamp: i64, seconds: i64, label: &str) -> Result<i64> {
        timestamp
            .checked_add(seconds)
            .ok_or_else(|| Error::Storage(format!("{label} timestamp overflow")))
    }
}

#[cfg(test)]
#[path = "../../tests/storage/reconciliation_test.rs"]
pub(crate) mod tests;

#[async_trait::async_trait]
pub trait ReconciliationEvidenceSource: Send + Sync {
    async fn lookup(&self, intent: &PaymentIntent, observed_at: i64) -> Result<ReconciliationEvidence>;
}

use async_trait::async_trait;
/// Evidence adapter for providers whose verification endpoint can report a
/// previously confirmed settlement. It deliberately invokes `Verify` only;
/// reconciliation never repeats the state-changing `Settle` operation.
pub struct VerifyOnlyProviderEvidence {
    provider: Arc<dyn ExternalSettlementProvider>,
    requests: Arc<dyn ProviderReconciliationRequest>,
}

impl VerifyOnlyProviderEvidence {
    #[must_use]
    pub fn new(
        provider: Arc<dyn ExternalSettlementProvider>,
        requests: Arc<dyn ProviderReconciliationRequest>,
    ) -> Self {
        Self { provider, requests }
    }
}

#[async_trait]
impl ReconciliationEvidenceSource for VerifyOnlyProviderEvidence {
    async fn lookup(&self, intent: &PaymentIntent, observed_at: i64) -> Result<ReconciliationEvidence> {
        let Some(request) = self.requests.build(intent)? else {
            return Ok(ReconciliationEvidence::Unavailable {
                reason: "provider does not expose a safe reconciliation query".to_owned(),
            });
        };
        let result = self.provider.execute(SettlementOperation::Verify, &request).await?;
        ProviderEvidence::provider_result_to_evidence(
            intent,
            observed_at,
            self.requests.authority(),
            self.requests.minimum_confirmations(),
            result,
        )
    }
}

pub struct ProviderEvidence;
impl ProviderEvidence {
    ///
    /// # Errors
    /// Returns a settlement error when the provider result lacks intent-bound confirmation evidence.
    pub fn provider_result_to_evidence(
        intent: &PaymentIntent,
        observed_at: i64,
        authority: &str,
        minimum_confirmations: u64,
        result: SettlementProviderResult,
    ) -> Result<ReconciliationEvidence> {
        if !result.valid {
            return Ok(ReconciliationEvidence::Unknown {
                reason: result
                    .invalid_reason
                    .unwrap_or_else(|| "provider could not validate the durable payment evidence".to_owned()),
            });
        }
        if !result.settled {
            if intent.status == PaymentStatus::Submitted {
                return Ok(ReconciliationEvidence::Verified {
                    verification_ref: crate::storage::reconciliation::SettlementEvidence::verification_ref(
                        &intent.id, &result,
                    )?,
                });
            }
            return Ok(ReconciliationEvidence::Pending);
        }
        result.validate_confirmed_for(intent, minimum_confirmations)?;
        let _observed_asset = result
            .asset
            .as_ref()
            .ok_or_else(|| Error::Settlement("provider result has no confirmed asset".to_owned()))?;
        let asset = intent.asset.clone();
        let settled_amount = result
            .amount
            .ok_or_else(|| Error::Settlement("provider result has no confirmed amount".to_owned()))?;
        let _observed_recipient = result
            .recipient
            .as_ref()
            .ok_or_else(|| Error::Settlement("provider result has no confirmed recipient".to_owned()))?;
        let recipient = intent.recipient.clone();
        let payer = result.payer;
        let confirmations = result.confirmations.unwrap_or_default();
        let tx_hash = ReconciliationPosting::nonempty(result.transaction);
        let settlement_ref = ReconciliationPosting::nonempty(result.settlement_ref);
        let effect_ref = ReconciliationPosting::nonempty(result.effect_ref);
        let posting_key = crate::model::PaymentLedgerEntry::settlement_posting_key(
            authority,
            &intent.asset.network,
            tx_hash.as_deref(),
            settlement_ref.as_deref(),
            effect_ref.as_deref(),
        )
        .map_err(|field| Error::Settlement(format!("invalid settlement posting evidence: {field}")))?;
        Ok(ReconciliationEvidence::Confirmed(Box::new(ConfirmedSettlement {
            authority: authority.to_owned(),
            asset,
            recipient,
            payer,
            settled_amount,
            posting_key,
            tx_hash,
            settlement_ref,
            effect_ref,
            confirmations,
            minimum_confirmations,
            effective_at: observed_at,
        })))
    }
}

/// Routes reconciliation by durable payment direction. An outbound protocol
/// never reuses the inbound provider settlement path: its merchant receipt is
/// only a transaction hint and must be independently proven by the fixed
/// operator RPC adapter configured for the intent's network.
pub struct PaymentEvidenceRouter {
    provider: Option<Arc<dyn ReconciliationEvidenceSource>>,
    outbound: Option<Arc<dyn ReconciliationEvidenceSource>>,
}

impl PaymentEvidenceRouter {
    #[must_use]
    pub fn new(
        provider: Arc<dyn ReconciliationEvidenceSource>,
        outbound: Arc<dyn ReconciliationEvidenceSource>,
    ) -> Self {
        Self {
            provider: Some(provider),
            outbound: Some(outbound),
        }
    }
}

#[async_trait]
impl ReconciliationEvidenceSource for PaymentEvidenceRouter {
    async fn lookup(&self, intent: &PaymentIntent, observed_at: i64) -> Result<ReconciliationEvidence> {
        if intent.direction != PaymentDirection::Outbound {
            return match &self.provider {
                Some(provider) => provider.lookup(intent, observed_at).await,
                None => Ok(ReconciliationEvidence::Unavailable {
                    reason: "inbound recovery is owned by the payee service".into(),
                }),
            };
        }
        match &self.outbound {
            Some(outbound) => outbound.lookup(intent, observed_at).await,
            None => Ok(ReconciliationEvidence::Unavailable {
                reason: "outbound recovery is owned by the payer service".into(),
            }),
        }
    }
}

/// Scheduler adapter; financial transitions and recovery belong to `PaymentReconciler`.
pub struct ReconciliationJob {
    processor: PaymentReconciler,
}
impl ReconciliationJob {
    /// # Errors
    /// Returns invalid worker configuration.
    pub fn new(
        storage: Arc<dyn IIntentRepository>,
        evidence: Arc<dyn ReconciliationEvidenceSource>,
        config: ReconciliationConfig,
    ) -> Result<Self> {
        Ok(Self {
            processor: PaymentReconciler::new(storage, evidence, config)?,
        })
    }
    /// # Errors
    /// Returns invalid worker configuration.
    pub fn with_clock(
        storage: Arc<dyn IIntentRepository>,
        evidence: Arc<dyn ReconciliationEvidenceSource>,
        clock: Arc<dyn UnixClock>,
        config: ReconciliationConfig,
    ) -> Result<Self> {
        Ok(Self {
            processor: PaymentReconciler::with_clock(storage, evidence, clock, config)?,
        })
    }
    /// # Errors
    /// Propagates a reconciliation pass failure.
    pub async fn run_once(&self) -> Result<ReconciliationRun> {
        self.processor.run_once().await
    }
}

impl PaymentEvidenceRouter {
    #[must_use]
    pub fn with_roles(
        provider: Option<Arc<dyn ReconciliationEvidenceSource>>,
        outbound: Option<Arc<dyn ReconciliationEvidenceSource>>,
    ) -> Self {
        Self { provider, outbound }
    }
}

impl PaymentReconciler {
    #[must_use]
    pub fn for_direction(mut self, direction: Option<PaymentDirection>) -> Self {
        self.direction = direction;
        self
    }
}
impl ReconciliationJob {
    #[must_use]
    pub fn for_direction(mut self, direction: Option<PaymentDirection>) -> Self {
        self.processor = self.processor.for_direction(direction);
        self
    }
}

mod reconciliation_model {
    use crate::model::{payment_recipients_match, Amount, AssetIdentity, PaymentIntent};
    use crate::{Error, Result};

    use crate::storage::reconciliation::SettlementProviderResult;

    /// Confirmed financial fact returned by a trusted provider or RPC adapter.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct ConfirmedSettlement {
        pub authority: String,
        pub asset: AssetIdentity,
        pub recipient: String,
        pub payer: Option<String>,
        pub settled_amount: Amount,
        pub posting_key: String,
        pub tx_hash: Option<String>,
        pub settlement_ref: Option<String>,
        pub effect_ref: Option<String>,
        pub confirmations: u64,
        pub minimum_confirmations: u64,
        pub effective_at: i64,
    }

    impl ConfirmedSettlement {
        ///
        /// # Errors
        /// Returns a settlement error when evidence does not match the durable payment intent.
        pub fn validate_for(&self, intent: &PaymentIntent, observed_at: i64) -> Result<()> {
            if self.authority.trim().is_empty() {
                return Err(Error::Settlement("settlement evidence authority is empty".to_owned()));
            }
            if self.posting_key.trim().is_empty() {
                return Err(Error::Settlement("settlement evidence posting key is empty".to_owned()));
            }
            let expected_posting_key = crate::model::PaymentLedgerEntry::settlement_posting_key(
                &self.authority,
                &intent.asset.network,
                self.tx_hash.as_deref(),
                self.settlement_ref.as_deref(),
                self.effect_ref.as_deref(),
            )
            .map_err(|field| Error::Settlement(format!("invalid settlement posting evidence: {field}")))?;
            if self.posting_key != expected_posting_key {
                return Err(Error::Settlement(
                    "settlement evidence posting key is not canonical".to_owned(),
                ));
            }
            if self.settled_amount != intent.amount {
                return Err(Error::Settlement(
                    "settlement evidence amount does not match the exact payment intent".to_owned(),
                ));
            }
            if self.asset != intent.asset
                || !payment_recipients_match(&intent.asset.network, &self.recipient, &intent.recipient)
                || self.confirmations < self.minimum_confirmations
            {
                return Err(Error::Settlement(
                    "settlement evidence asset, recipient, or confirmations do not match the intent".to_owned(),
                ));
            }
            if self.tx_hash.as_deref().is_none_or(|value| value.trim().is_empty())
                && self
                    .settlement_ref
                    .as_deref()
                    .is_none_or(|value| value.trim().is_empty())
            {
                return Err(Error::Settlement(
                    "settlement evidence has no transaction or provider reference".to_owned(),
                ));
            }
            crate::model::PaymentLedgerEntry::validate_trusted_effective_time(
                intent.created_at,
                self.effective_at,
                observed_at,
            )?;
            if intent
                .tx_hash
                .as_deref()
                .is_some_and(|expected| self.tx_hash.as_deref() != Some(expected))
            {
                return Err(Error::Settlement(
                    "settlement evidence transaction conflicts with the payment intent".to_owned(),
                ));
            }
            if intent
                .settlement_ref
                .as_deref()
                .is_some_and(|expected| self.settlement_ref.as_deref() != Some(expected))
            {
                return Err(Error::Settlement(
                    "settlement evidence reference conflicts with the payment intent".to_owned(),
                ));
            }
            Ok(())
        }
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum ReconciliationEvidence {
        Confirmed(Box<ConfirmedSettlement>),
        Verified { verification_ref: String },
        Pending,
        Expired { reason: String },
        Unknown { reason: String },
        Unavailable { reason: String },
    }

    /// Trusted provider observation plus the operator-owned evidence authority.
    /// This crate-private value joins settlement recovery to reconciliation and
    /// is never serialized as a transport DTO.
    pub enum ReconciliationProviderObservation {
        Observed {
            authority: String,
            minimum_confirmations: u64,
            result: Box<SettlementProviderResult>,
            effective_at: Option<i64>,
        },
        Expired {
            reason: String,
        },
    }

    /// Summary for one bounded reconciliation pass.
    #[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
    pub struct ReconciliationRun {
        pub claimed: u32,
        pub settled: u32,
        pub verified: u32,
        pub expired: u32,
        pub pending: u32,
        pub unknown: u32,
        pub unavailable: u32,
        pub provider_errors: u32,
        pub skipped: u32,
    }

    #[cfg(test)]
    mod tests {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/model/reconciliation_test.rs"
        ));
    }
}
pub use reconciliation_model::*;

mod settlement_model {
    use std::collections::BTreeMap;

    use serde::{Deserialize, Serialize};
    use serde_json::Value;

    use crate::model::{payment_recipients_match, Amount, AssetIdentity, PaymentIntent};
    use crate::{Error, Result};

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub enum SettlementOperation {
        Verify,
        Settle,
    }

    #[derive(Clone, Debug, Serialize)]
    #[serde(rename_all = "camelCase")]
    pub struct SettlementProviderRequest {
        pub protocol: String,
        pub run_id: String,
        pub payment_payload: Value,
        pub payment_requirements: Value,
    }

    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
    #[serde(rename_all = "camelCase", deny_unknown_fields)]
    pub struct SettlementProviderResult {
        #[serde(default)]
        pub valid: bool,
        #[serde(default)]
        pub settled: bool,
        #[serde(default)]
        pub payer: Option<String>,
        #[serde(default)]
        pub transaction: Option<String>,
        #[serde(default)]
        pub settlement_ref: Option<String>,
        #[serde(default)]
        pub effect_ref: Option<String>,
        #[serde(default)]
        pub invalid_reason: Option<String>,
        #[serde(default)]
        pub asset: Option<AssetIdentity>,
        #[serde(default)]
        pub amount: Option<Amount>,
        #[serde(default)]
        pub recipient: Option<String>,
        #[serde(default)]
        pub confirmations: Option<u64>,
        #[serde(default)]
        pub extensions: BTreeMap<String, Value>,
    }

    impl SettlementProviderResult {
        ///
        /// # Errors
        /// Returns a settlement error for mismatched assets, recipients, amounts or confirmation depth.
        pub fn validate_confirmed_for(&self, intent: &PaymentIntent, minimum_confirmations: u64) -> Result<()> {
            let asset_matches = self
                .asset
                .as_ref()
                .is_some_and(|asset| asset_identity_matches(asset, &intent.asset));
            let recipient_matches = self
                .recipient
                .as_deref()
                .is_some_and(|recipient| payment_recipients_match(&intent.asset.network, recipient, &intent.recipient));
            let confirmations_match =
                minimum_confirmations == 0 || self.confirmations.is_some_and(|value| value >= minimum_confirmations);
            if !self.valid
                || !self.settled
                || !asset_matches
                || self.amount != Some(intent.amount)
                || !recipient_matches
                || !confirmations_match
                || (self.transaction.as_deref().is_none_or(str::is_empty)
                    && self.settlement_ref.as_deref().is_none_or(str::is_empty))
            {
                return Err(Error::Settlement(
                    "provider settlement evidence does not match the payment intent".to_owned(),
                ));
            }
            Ok(())
        }
    }

    fn asset_identity_matches(observed: &AssetIdentity, expected: &AssetIdentity) -> bool {
        observed.network == expected.network
            && observed.asset.eq_ignore_ascii_case(&expected.asset)
            && observed.decimals == expected.decimals
            && match (&observed.token_contract, &expected.token_contract) {
                (Some(observed), Some(expected)) => observed.eq_ignore_ascii_case(expected),
                (None, None) => true,
                _ => false,
            }
    }

    #[async_trait::async_trait]
    pub trait ExternalSettlementProvider: Send + Sync {
        async fn execute(
            &self,
            operation: SettlementOperation,
            request: &SettlementProviderRequest,
        ) -> Result<SettlementProviderResult>;
    }

    /// Builds a read-only provider verification request from durable state.
    /// Returning `None` explicitly declares that this provider cannot safely
    /// recover this intent through a query API.
    pub trait ProviderReconciliationRequest: Send + Sync {
        fn authority(&self) -> &str;

        fn minimum_confirmations(&self) -> u64 {
            1
        }

        /// Reconstructs the provider-specific verification request exclusively
        /// from durable data.
        ///
        /// # Errors
        ///
        /// Returns an error when durable protocol data is incomplete or corrupt.
        fn build(&self, intent: &PaymentIntent) -> Result<Option<SettlementProviderRequest>>;
    }

    pub struct SettlementEvidence;
    impl SettlementEvidence {
        ///
        /// # Errors
        /// Returns the validation, cryptographic or storage failure from the requested operation.
        pub fn hash(value: &serde_json::Value) -> Result<String> {
            use sha2::{Digest as _, Sha256};
            let encoded = serde_json::to_vec(value)?;
            let mut hash = Sha256::new();
            hash.update(b"agentvault.settlement-evidence.v1");
            hash.update(u64::try_from(encoded.len()).unwrap_or(u64::MAX).to_be_bytes());
            hash.update(encoded);
            Ok(hex::encode(hash.finalize()))
        }
        ///
        /// # Errors
        /// Returns a serialization error if the provider observation cannot be encoded.
        pub fn verification_ref(intent_id: &str, result: &SettlementProviderResult) -> Result<String> {
            Self::hash(&serde_json::json!({"intent_id":intent_id,"operation":"verify","result":result}))
        }
    }
    #[cfg(test)]
    mod tests {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/model/settlement_test.rs"));
    }
}
pub use settlement_model::*;
