use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use serde_json::json;
use sha2::{Digest, Sha256};
use tracing::Instrument as _;

use crate::{
    handler::{
        provider_verification_ref, ExternalSettlementProvider, OutboundEvidenceHandler, SettlementHandler,
        SettlementOperation, SettlementProviderRequest, SettlementProviderResult,
    },
    model::{
        settlement_posting_key, ConfirmedSettlement, LedgerEntryType, OutboundEvidenceObservation, PaymentDirection,
        PaymentEvent, PaymentEventType, PaymentIntent, PaymentLedgerEntry, PaymentReceipt, PaymentStatus,
        ReconciliationEvidence, ReconciliationProviderObservation, ReconciliationRun, ReservationStatus,
    },
    storage::{
        OutboxMessage, OutboxStatus, PaymentProgressWrite, PaymentStorage, ReconciliationLeaseRequest, SettlementWrite,
    },
    Error, Result,
};

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
        duration_seconds(self.lease_duration, "lease_duration")?;
        let initial = duration_seconds(self.initial_backoff, "initial_backoff")?;
        let maximum = duration_seconds(self.max_backoff, "max_backoff")?;
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
#[async_trait]
pub trait ReconciliationEvidenceSource: Send + Sync {
    async fn lookup(&self, intent: &PaymentIntent, observed_at: i64) -> Result<ReconciliationEvidence>;
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
        provider_result_to_evidence(
            intent,
            observed_at,
            self.requests.authority(),
            self.requests.minimum_confirmations(),
            result,
        )
    }
}

#[async_trait]
impl ReconciliationEvidenceSource for SettlementHandler {
    async fn lookup(&self, intent: &PaymentIntent, observed_at: i64) -> Result<ReconciliationEvidence> {
        match self.reconciliation_observation(intent, observed_at).await? {
            ReconciliationProviderObservation::Observed {
                authority,
                minimum_confirmations,
                result,
                effective_at,
            } => provider_result_to_evidence(
                intent,
                effective_at.unwrap_or(observed_at),
                &authority,
                minimum_confirmations,
                *result,
            ),
            ReconciliationProviderObservation::Expired { reason } => Ok(ReconciliationEvidence::Expired { reason }),
        }
    }
}

/// Routes reconciliation by durable payment direction. An outbound protocol
/// never reuses the inbound provider settlement path: its merchant receipt is
/// only a transaction hint and must be independently proven by the fixed
/// operator RPC adapter configured for the intent's network.
pub struct PaymentEvidenceRouter {
    provider: Arc<dyn ReconciliationEvidenceSource>,
    outbound: Arc<OutboundEvidenceHandler>,
}

impl PaymentEvidenceRouter {
    #[must_use]
    pub fn new(provider: Arc<dyn ReconciliationEvidenceSource>, outbound: Arc<OutboundEvidenceHandler>) -> Self {
        Self { provider, outbound }
    }
}

#[async_trait]
impl ReconciliationEvidenceSource for PaymentEvidenceRouter {
    async fn lookup(&self, intent: &PaymentIntent, observed_at: i64) -> Result<ReconciliationEvidence> {
        if intent.direction != PaymentDirection::Outbound {
            return self.provider.lookup(intent, observed_at).await;
        }
        match self.outbound.reconciliation_observation(intent).await? {
            OutboundEvidenceObservation::Confirmed {
                authority,
                transaction,
                payer,
                effect_ref,
                confirmations,
                minimum_confirmations,
                effective_at,
            } => {
                let posting_key = settlement_posting_key(
                    &authority,
                    &intent.asset.network,
                    Some(&transaction),
                    None,
                    Some(&effect_ref),
                )
                .map_err(|field| Error::Settlement(format!("invalid RPC settlement evidence: {field}")))?;
                Ok(ReconciliationEvidence::Confirmed(Box::new(ConfirmedSettlement {
                    authority,
                    asset: intent.asset.clone(),
                    recipient: intent.recipient.clone(),
                    payer: Some(payer),
                    settled_amount: intent.amount,
                    posting_key,
                    tx_hash: Some(transaction),
                    settlement_ref: None,
                    effect_ref: Some(effect_ref),
                    confirmations,
                    minimum_confirmations,
                    effective_at,
                })))
            }
            OutboundEvidenceObservation::Pending => Ok(ReconciliationEvidence::Pending),
            OutboundEvidenceObservation::Unknown { reason } => Ok(ReconciliationEvidence::Unknown { reason }),
            OutboundEvidenceObservation::Unavailable { reason } => Ok(ReconciliationEvidence::Unavailable { reason }),
        }
    }
}

fn provider_result_to_evidence(
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
                verification_ref: provider_verification_ref(&intent.id, &result)?,
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
    let tx_hash = nonempty(result.transaction);
    let settlement_ref = nonempty(result.settlement_ref);
    let effect_ref = nonempty(result.effect_ref);
    let posting_key = settlement_posting_key(
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

/// Injectable wall clock used for deterministic lease and retry tests.
pub trait UnixClock: Send + Sync {
    /// Returns seconds since the Unix epoch.
    ///
    /// # Errors
    ///
    /// Returns an error if the host clock is before the Unix epoch or outside
    /// the persisted timestamp range.
    fn now(&self) -> Result<i64>;
}

#[derive(Debug, Default)]
pub(super) struct SystemUnixClock;

impl UnixClock for SystemUnixClock {
    fn now(&self) -> Result<i64> {
        let seconds = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_err(|error| Error::Settlement(format!("system clock is before Unix epoch: {error}")))?
            .as_secs();
        i64::try_from(seconds).map_err(|_| Error::Settlement("system clock exceeds timestamp range".to_owned()))
    }
}

/// Claims and reconciles one durable batch. Database-specific locking remains
/// behind `PaymentStorage`: `PostgreSQL` adapters can use `SKIP LOCKED`, while
/// `SQLite` adapters can use a short conditional lease transaction.
pub struct ReconciliationJob {
    storage: Arc<dyn PaymentStorage>,
    evidence: Arc<dyn ReconciliationEvidenceSource>,
    clock: Arc<dyn UnixClock>,
    config: ReconciliationConfig,
    lease_seconds: i64,
    initial_backoff_seconds: i64,
    max_backoff_seconds: i64,
}

impl ReconciliationJob {
    /// Builds a worker using the host wall clock.
    ///
    /// # Errors
    ///
    /// Returns an error when the worker configuration is invalid.
    pub fn new(
        storage: Arc<dyn PaymentStorage>,
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
        storage: Arc<dyn PaymentStorage>,
        evidence: Arc<dyn ReconciliationEvidenceSource>,
        clock: Arc<dyn UnixClock>,
        config: ReconciliationConfig,
    ) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            storage,
            evidence,
            clock,
            lease_seconds: duration_seconds(config.lease_duration, "lease_duration")?,
            initial_backoff_seconds: duration_seconds(config.initial_backoff, "initial_backoff")?,
            max_backoff_seconds: duration_seconds(config.max_backoff, "max_backoff")?,
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
            owner: self.config.worker_id.clone(),
            now: claim_now,
            lease_until: checked_timestamp_add(claim_now, self.lease_seconds, "reconciliation lease")?,
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
                        checked_timestamp_add(now, self.lease_seconds, "reconciliation lease")?,
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
            id: stable_id("event", &material),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::Failed,
            attempt_no,
            signature_ref: None,
            tx_hash: None,
            payload: json!({"stage": stage, "reason": reason}),
            created_at: updated_at,
        };
        let outbox = OutboxMessage {
            id: stable_id("outbox", &material),
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
        let commit = build_reconciliation_commit(intent, evidence, attempt_no, now)?;
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
        verified.next_reconcile_at = Some(checked_timestamp_add(
            updated_at,
            self.initial_backoff_seconds,
            "verified settlement retry",
        )?);
        let event = PaymentEvent {
            id: stable_id("event", &format!("{}:verified:{verification_ref}", intent.id)),
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
                checked_timestamp_add(now, delay, "reconciliation retry")?,
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

struct ReconciliationCommit {
    intent: PaymentIntent,
    ledger: PaymentLedgerEntry,
    event: PaymentEvent,
    outbox: OutboxMessage,
}

fn build_reconciliation_commit(
    intent: &PaymentIntent,
    evidence: &ConfirmedSettlement,
    attempt_no: u32,
    now: i64,
) -> Result<ReconciliationCommit> {
    let wallet_id = intent
        .wallet_id
        .as_deref()
        .ok_or_else(|| Error::Settlement("settled intent has no wallet".to_owned()))?;
    let stable_material = format!("{}:{}:{}", wallet_id, intent.id, evidence.posting_key);
    let ledger_id = stable_id("ledger", &stable_material);
    let receipt = reconciled_receipt(intent, evidence)?;
    Ok(ReconciliationCommit {
        intent: reconciled_intent(intent, evidence, now),
        ledger: reconciled_ledger(intent, evidence, wallet_id, &ledger_id, now)?,
        event: reconciled_event(
            intent,
            evidence,
            &receipt,
            &ledger_id,
            &stable_material,
            attempt_no,
            now,
        ),
        outbox: reconciled_outbox(intent, evidence, &stable_material, now),
    })
}

fn reconciled_intent(intent: &PaymentIntent, evidence: &ConfirmedSettlement, now: i64) -> PaymentIntent {
    let mut settled = intent.clone();
    settled.status = PaymentStatus::Settled;
    settled.settled_amount = evidence.settled_amount;
    settled.tx_hash.clone_from(&evidence.tx_hash);
    settled.settlement_ref.clone_from(&evidence.settlement_ref);
    settled.next_reconcile_at = None;
    // The immutable ledger/receipt retain the trusted chain timestamp. The
    // intent lifecycle itself cannot predate local intent creation when the
    // two clocks differ within the accepted bounded-skew window.
    settled.settled_at = Some(evidence.effective_at.max(intent.created_at));
    settled.updated_at = now.max(intent.updated_at.saturating_add(1));
    if settled.reservation_status == ReservationStatus::Reserved {
        settled.reservation_status = ReservationStatus::Consumed;
    }
    settled
}

fn reconciled_ledger(
    intent: &PaymentIntent,
    evidence: &ConfirmedSettlement,
    wallet_id: &str,
    ledger_id: &str,
    now: i64,
) -> Result<PaymentLedgerEntry> {
    let ledger = PaymentLedgerEntry {
        id: ledger_id.to_owned(),
        intent_id: Some(intent.id.clone()),
        scenario_id: Some(intent.scenario_id.clone()),
        wallet_id: wallet_id.to_owned(),
        entry_type: LedgerEntryType::Payment,
        direction: intent.direction,
        asset: intent.asset.clone(),
        amount: evidence.settled_amount,
        posting_key: evidence.posting_key.clone(),
        tx_hash: evidence.tx_hash.clone(),
        settlement_ref: evidence.settlement_ref.clone(),
        effect_ref: evidence.effect_ref.clone(),
        transfer_ref: None,
        reversal_of: None,
        effective_at: evidence.effective_at,
        created_at: now,
    };
    ledger
        .validate()
        .map_err(|field| Error::Settlement(format!("invalid reconciled ledger field: {field}")))?;
    Ok(ledger)
}

fn reconciled_receipt(intent: &PaymentIntent, evidence: &ConfirmedSettlement) -> Result<PaymentReceipt> {
    let evidence_hash = stable_id(
        "evidence",
        &format!(
            "{}:{}:{:?}:{:?}:{}",
            evidence.authority,
            evidence.posting_key,
            evidence.tx_hash,
            evidence.settlement_ref,
            evidence.settled_amount
        ),
    );
    let receipt = PaymentReceipt {
        intent_id: intent.id.clone(),
        scenario_id: intent.scenario_id.clone(),
        direction: intent.direction,
        protocol: intent.protocol.clone(),
        asset: intent.asset.clone(),
        recipient: intent.recipient.clone(),
        payer: evidence.payer.clone(),
        requested_amount: intent.amount,
        settled_amount: evidence.settled_amount,
        status: PaymentStatus::Settled,
        tx_hash: evidence.tx_hash.clone(),
        settlement_ref: evidence.settlement_ref.clone(),
        evidence_hash,
        confirmed_at: evidence.effective_at,
    };
    receipt
        .validate()
        .map_err(|field| Error::Settlement(format!("invalid reconciled receipt field: {field}")))?;
    Ok(receipt)
}

fn reconciled_event(
    intent: &PaymentIntent,
    evidence: &ConfirmedSettlement,
    receipt: &PaymentReceipt,
    ledger_id: &str,
    stable_material: &str,
    attempt_no: u32,
    now: i64,
) -> PaymentEvent {
    PaymentEvent {
        id: stable_id("event", stable_material),
        intent_id: intent.id.clone(),
        event_type: PaymentEventType::Reconciled,
        attempt_no,
        signature_ref: None,
        tx_hash: evidence.tx_hash.clone(),
        payload: json!({
            "authority": evidence.authority,
            "posting_key": evidence.posting_key,
            "ledger_id": ledger_id,
            "receipt": receipt,
        }),
        created_at: now,
    }
}

fn reconciled_outbox(
    intent: &PaymentIntent,
    evidence: &ConfirmedSettlement,
    stable_material: &str,
    now: i64,
) -> OutboxMessage {
    OutboxMessage {
        id: stable_id("outbox", stable_material),
        aggregate_type: "payment_intent".to_owned(),
        aggregate_id: intent.id.clone(),
        event_type: "payment_settled".to_owned(),
        destination: format!("scenario:{}.payments", intent.scenario_id),
        run_id: intent.run_id.clone(),
        payload: json!({
            "intent_id": intent.id,
            "scenario_id": intent.scenario_id,
            "status": PaymentStatus::Settled.as_str(),
            "settled_amount": evidence.settled_amount,
            "tx_hash": evidence.tx_hash,
            "settlement_ref": evidence.settlement_ref,
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

fn duration_seconds(duration: Duration, name: &str) -> Result<i64> {
    let seconds = duration.as_secs();
    if duration.is_zero() || duration.subsec_nanos() != 0 || i64::try_from(seconds).is_err() {
        return Err(Error::Config(format!(
            "reconciliation {name} must be a positive whole number of seconds"
        )));
    }
    i64::try_from(seconds).map_err(|_| Error::Config(format!("reconciliation {name} exceeds timestamp range")))
}

fn checked_timestamp_add(timestamp: i64, seconds: i64, label: &str) -> Result<i64> {
    timestamp
        .checked_add(seconds)
        .ok_or_else(|| Error::Storage(format!("{label} timestamp overflow")))
}

fn nonempty(value: Option<String>) -> Option<String> {
    value.filter(|candidate| !candidate.trim().is_empty())
}

fn stable_id(prefix: &str, material: &str) -> String {
    let digest = Sha256::digest(material.as_bytes());
    format!("{prefix}-{}", hex::encode(digest))
}

#[cfg(test)]
#[path = "../../tests/handler/reconciliation_test.rs"]
mod tests;
