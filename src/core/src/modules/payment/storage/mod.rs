//! Payment persistence contract.

use crate::{
    model::Amount, model::ApprovalRequest, model::BudgetUnit, model::PaymentChallenge, model::PaymentClient,
    model::PaymentCredential, model::PaymentDirection, model::PaymentEvent, model::PaymentIntent,
    model::PaymentLedgerEntry, model::PaymentPolicy, model::PaymentPrice, model::PaymentScenario, model::PaymentStatus,
    storage::EntityComparison, storage::EntityQuery, storage::EntityValue, storage::IdempotentWrite,
    storage::OutboxMessage, storage::Page, storage::PageRequest, storage::PaymentWallet, Result,
};
use async_trait::async_trait;

pub(crate) mod r#macro;
pub mod postgres;
pub mod sqlite;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaymentQuery {
    pub scenario_id: Option<String>,
    pub wallet_id: Option<String>,
    pub run_id: Option<String>,
    pub direction: Option<PaymentDirection>,
    pub status: Option<PaymentStatus>,
    pub from_created_at: Option<i64>,
    pub to_created_at: Option<i64>,
    pub page: PageRequest,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconciliationLeaseRequest {
    pub direction: Option<PaymentDirection>,
    pub owner: String,
    pub now: i64,
    pub lease_until: i64,
    pub limit: u32,
}

/// One durably leased reconciliation attempt. The counter is assigned
/// atomically with the lease so retries across workers and process
/// restarts cannot reuse an operational attempt number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReconciliationClaim {
    pub intent: PaymentIntent,
    pub attempt_no: u32,
}

/// Atomic settlement/reconciliation commit. The intent transition, operation
/// events, confirmed financial facts, and outgoing notifications either all
/// commit or all roll back. For an outbound reserved intent this is the only
/// valid path to `consumed`: the transaction must append its trusted ledger
/// facts and consume the reservation together.
pub struct SettlementWrite<'a> {
    pub intent: &'a PaymentIntent,
    pub expected_updated_at: i64,
    pub events: &'a [PaymentEvent],
    pub ledger_entries: &'a [PaymentLedgerEntry],
    pub outbox: &'a [OutboxMessage],
}

/// One durable non-financial lifecycle transition. This path records provider
/// verification, retry, failure, or expiry while preserving an outstanding
/// reservation (or releasing it on a terminal failure). It can never produce
/// a signature, consume a reservation, mark an intent settled, or post money.
pub struct PaymentProgressWrite<'a> {
    pub intent: &'a PaymentIntent,
    pub expected_updated_at: i64,
    pub events: &'a [PaymentEvent],
    pub outbox: &'a [OutboxMessage],
}

/// One untrusted merchant settlement hint for an outbound x402 intent.
///
/// The transaction may only move `Signed`/`Unknown` to
/// `PendingSettlement`, preserve the complete reservation and every financial
/// field, and append one `Submitted` event whose transaction locator remains
/// payload-only. In particular this boundary cannot populate the intent's
/// trusted `tx_hash`, consume budget, or post a ledger entry.
pub struct OutboundEvidenceWrite<'a> {
    pub intent: &'a PaymentIntent,
    pub expected_updated_at: i64,
    pub evidence_hash: &'a str,
    pub event: &'a PaymentEvent,
    pub outbox: &'a [OutboxMessage],
}

/// One payee challenge issuance transaction. The complete challenge is also
/// carried by an immutable `ChallengeCreated` event so verification and crash
/// recovery do not depend on process memory.
pub struct ChallengeWrite<'a> {
    pub intent: &'a PaymentIntent,
    pub challenge: &'a PaymentChallenge,
    pub guard: &'a ChallengeIssuanceGuard,
    pub events: &'a [PaymentEvent],
    pub outbox: &'a [OutboxMessage],
}

/// Trusted snapshots that must still match while an inbound challenge is
/// inserted. This closes the lifecycle/price/recipient race between preflight
/// reads and durable challenge issuance.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChallengeIssuanceGuard {
    pub principal: String,
    pub client: PaymentClient,
    pub scenario: PaymentScenario,
    pub price: PaymentPrice,
    pub policy: PaymentPolicy,
    pub wallet: PaymentWallet,
    pub origin: String,
    pub day_started_at: i64,
    pub evaluated_at: i64,
}

/// One inbound credential verification transition. Implementations bind the
/// credential fingerprint, update the intent, append events, and enqueue
/// notifications in one database transaction. The unique protocol/credential
/// index prevents a credential being redeemed by another request.
pub struct VerificationWrite<'a> {
    pub intent: &'a PaymentIntent,
    pub credential: &'a PaymentCredential,
    pub expected_updated_at: i64,
    pub events: &'a [PaymentEvent],
    pub outbox: &'a [OutboxMessage],
}

/// One authorization transaction. With an approval row, the intent is stored
/// without a reservation. Without one, policy and shared-wallet budgets are
/// reserved before the intent becomes authorized. Events and notifications
/// commit with the aggregate in both cases.
pub struct AuthorizationWrite<'a> {
    pub intent: &'a PaymentIntent,
    pub approval: Option<&'a ApprovalRequest>,
    pub action: AuthorizationAction,
    pub guard: &'a AuthorizationGuard,
    pub events: &'a [PaymentEvent],
    pub outbox: &'a [OutboxMessage],
}

/// Snapshots and normalized request data that a storage adapter must validate
/// again while holding the rows used by an authorization transaction.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizationGuard {
    pub principal: String,
    pub client: PaymentClient,
    pub scenario: PaymentScenario,
    pub policy: PaymentPolicy,
    pub expected_wallet_updated_at: i64,
    pub expected_wallet_key_version: Option<u32>,
    pub origin: String,
    pub day_started_at: i64,
    pub evaluated_at: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthorizationAction {
    RecordDenied,
    RequestApproval,
    Reserve,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuthorizationRecord {
    pub intent: PaymentIntent,
    pub approval: Option<ApprovalRequest>,
}

/// Authoritative pending reservations plus same-day confirmed flow and
/// wallet-wide hard limits. Pending usage includes every `reserved` intent in
/// the requested direction regardless of creation day. Confirmed usage comes
/// only from same-direction ledger facts
/// whose `effective_at` is in the current UTC day. Both paths aggregate by
/// scenario (across every current or historical policy) or wallet plus
/// direction and budget asset+decimals,
/// never network or token contract.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BudgetSnapshot {
    pub budget_unit: BudgetUnit,
    pub wallet_enabled: bool,
    pub wallet_algorithm: String,
    pub wallet_address: String,
    pub wallet_updated_at: i64,
    pub wallet_active_key_version: Option<u32>,
    /// Whether enabled policies from more than one scenario share the wallet.
    pub wallet_shared_across_scenarios: bool,
    pub scenario_spent_and_reserved: Amount,
    pub wallet_spent_and_reserved: Amount,
    pub wallet_max_per_payment: Option<Amount>,
    pub wallet_max_per_day: Option<Amount>,
}

impl BudgetSnapshot {
    #[must_use]
    pub const fn empty(budget_unit: BudgetUnit) -> Self {
        Self {
            budget_unit,
            wallet_enabled: false,
            wallet_algorithm: String::new(),
            wallet_address: String::new(),
            wallet_updated_at: 0,
            wallet_active_key_version: None,
            wallet_shared_across_scenarios: false,
            scenario_spent_and_reserved: Amount::ZERO,
            wallet_spent_and_reserved: Amount::ZERO,
            wallet_max_per_payment: None,
            wallet_max_per_day: None,
        }
    }
}

#[async_trait]
pub trait IIntentRepository: Send + Sync {
    /// Looks up the authoritative replay identity before re-evaluating mutable
    /// policy. Callers compare `request_hash` and reject mismatches.
    async fn find_run(&self, scenario_id: &str, run_id: &str) -> Result<Option<AuthorizationRecord>>;

    /// Atomically inserts an authorization aggregate, reserves both policy and
    /// wallet budgets when no approval is needed, and appends its events and
    /// outbox messages. The same transaction must lock/reload the client,
    /// scenario, policy, wallet, and active key row; verify their enabled
    /// state, ownership, local custody, active non-compromised signing key,
    /// snapshot revisions, request scope, challenge expiry, matching
    /// intent/policy/wallet budget unit, and current hard limits. A wallet
    /// shared by enabled policies from multiple scenarios must have both
    /// wallet-wide limits configured; missing limits fail closed. The
    /// transaction then
    /// recompute both budgets as all outstanding same-direction `reserved`
    /// intents in the unit (including prior days) plus current-day confirmed
    /// same-direction ledger facts by `effective_at`. `created_at` is never the
    /// confirmed-flow clock, and network/token contract must not partition
    /// wallet usage. The
    /// active key version must match the preflight snapshot. External-custody
    /// wallets cannot authorize outbound payments. Any concurrent lifecycle,
    /// key, or policy change
    /// fails closed rather than authorizing from stale preflight data.
    async fn authorize(&self, write: AuthorizationWrite<'_>) -> Result<IdempotentWrite<AuthorizationRecord>>;

    /// Returns usage in `budget_unit`: all matching `direction` reservations,
    /// without a `created_at` cutoff, plus confirmed same-direction ledger
    /// facts where `day_started_at <= effective_at <= now + bounded trusted
    /// settlement clock skew`. Scenario usage
    /// spans every current or historical policy (and wallet) for `scenario_id`;
    /// policy rotation must never reset it. Wallet usage spans every scenario. A complete
    /// [`crate::model::AssetIdentity`] must never be the grouping key. Effective
    /// times beyond the shared trusted-settlement clock-skew bound, or any
    /// matching `consumed` intent without its durable same-direction ledger fact,
    /// are invariant violations and must fail closed. Facts inside the bound are
    /// included immediately so confirmed spending cannot bypass a budget while
    /// the application clock catches up.
    async fn budget_snapshot(
        &self,
        scenario_id: &str,
        wallet_id: &str,
        direction: PaymentDirection,
        budget_unit: &BudgetUnit,
        day_started_at: i64,
        now: i64,
    ) -> Result<BudgetSnapshot>;

    /// Atomically records an inbound intent, reserves the policy's inbound
    /// sales/receipt budget, and appends its immutable challenge event.
    /// The transaction must lock/reload and revalidate the guard's client,
    /// scenario, price, policy, policy-wallet membership, and wallet enabled
    /// state, ownership, revision/content, resource/origin,
    /// price/asset/protocol, directional limits, current inbound usage, and
    /// canonical recipient. Replays are accepted only when the stored request
    /// hash is identical.
    async fn record_challenge(&self, write: ChallengeWrite<'_>) -> Result<IdempotentWrite<PaymentIntent>>;

    /// Resolves a challenge only inside the authenticated scenario boundary.
    async fn get_for_challenge(&self, scenario_id: &str, challenge_hash: &str) -> Result<Option<PaymentIntent>>;

    /// Atomically binds a verified credential and advances the inbound intent.
    async fn verify_incoming(&self, write: VerificationWrite<'_>) -> Result<PaymentIntent>;

    async fn get(&self, intent_id: &str) -> Result<Option<PaymentIntent>>;

    /// Ownership-aware lookup used after authenticating the calling client.
    async fn get_for_scenario(&self, scenario_id: &str, intent_id: &str) -> Result<Option<PaymentIntent>>;

    /// Atomically advances a non-financial payment state and appends its
    /// operation events/outbox notifications. Implementations must reject
    /// `Signed`, `Settled`, any increase in settled money, and every
    /// reservation mutation except `Reserved -> Released` on `Failed` or
    /// `Expired`. Payment signatures and confirmed funds have dedicated
    /// transaction ports and cannot be expressed here.
    async fn commit_progress(&self, write: PaymentProgressWrite<'_>) -> Result<PaymentIntent>;

    /// Atomically records one idempotent protocol-owned outbound merchant-response hint.
    /// A byte-for-byte equivalent semantic hint is a replay; another hint for
    /// the same intent is a conflict. Only independently confirmed RPC
    /// evidence may subsequently call [`Self::commit_settlement`].
    async fn record_outbound_evidence(
        &self,
        write: OutboundEvidenceWrite<'_>,
    ) -> Result<IdempotentWrite<PaymentIntent>>;

    async fn list(&self, query: &PaymentQuery) -> Result<Page<PaymentIntent>>;

    /// Claims due Signed/Submitted/Pending/Unknown recovery work and expired
    /// Created challenges. Leases must be persisted before rows are returned;
    /// Created rows are eligible only when their durable `expires_at` is due.
    async fn claim_reconciliation(&self, request: &ReconciliationLeaseRequest) -> Result<Vec<ReconciliationClaim>>;

    async fn renew_reconciliation_lease(&self, intent_id: &str, owner: &str, lease_until: i64) -> Result<()>;

    async fn release_reconciliation_lease(&self, intent_id: &str, owner: &str, next_attempt_at: i64) -> Result<()>;

    /// Commits settlement atomically. Any reserved intent must become
    /// `consumed` together with at least one matching, idempotently posted
    /// same-direction ledger fact. A consumed intent without that fact, or a replay
    /// whose ledger content differs, is an invariant error and fails closed.
    async fn commit_settlement(&self, write: SettlementWrite<'_>) -> Result<PaymentIntent>;
}

impl OutboundEvidenceWrite<'_> {
    pub(crate) fn validate_outbound_evidence_transition(
        existing: &PaymentIntent,
        candidate: &PaymentIntent,
        expected_updated_at: i64,
        event: &PaymentEvent,
        evidence_hash: &str,
    ) -> Result<()> {
        let hash_valid = evidence_hash.len() == 64 && evidence_hash.bytes().all(|byte| byte.is_ascii_hexdigit());
        let payload = event.payload.get("merchant_response");
        let payload_bound = event.payload.get("evidence_hash").and_then(serde_json::Value::as_str)
            == Some(evidence_hash)
            && payload
                .and_then(|value| value.get("success"))
                .and_then(serde_json::Value::as_bool)
                == Some(true)
            && payload
                .and_then(|value| value.get("protocol"))
                .and_then(serde_json::Value::as_str)
                == Some(existing.protocol.as_str())
            && payload
                .and_then(|value| value.get("transaction"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty())
            && payload
                .and_then(|value| value.get("network"))
                .and_then(serde_json::Value::as_str)
                == Some(existing.asset.network.as_str())
            && payload
                .and_then(|value| value.get("payer"))
                .and_then(serde_json::Value::as_str)
                .is_some_and(|value| !value.is_empty());
        if !hash_valid
            || existing.updated_at != expected_updated_at
            || existing.direction != PaymentDirection::Outbound
            || existing.protocol.trim().is_empty()
            || !matches!(existing.status, PaymentStatus::Signed | PaymentStatus::Unknown)
            || candidate.status != PaymentStatus::PendingSettlement
            || !existing.same_immutable_identity_as(candidate)
            || candidate.updated_at <= expected_updated_at
            || candidate.reservation_status != existing.reservation_status
            || candidate.reserved_amount != existing.reserved_amount
            || existing.reservation_status != crate::model::ReservationStatus::Reserved
            || existing.reserved_amount != existing.amount
            || candidate.settled_amount != existing.settled_amount
            || candidate.tx_hash != existing.tx_hash
            || candidate.settlement_ref != existing.settlement_ref
            || candidate.settled_at != existing.settled_at
            || candidate.next_reconcile_at.is_none()
            || event.intent_id != existing.id
            || event.event_type != crate::model::PaymentEventType::Submitted
            || event.attempt_no != 0
            || event.signature_ref.is_some()
            || event.tx_hash.is_some()
            || event.created_at != candidate.updated_at
            || !payload_bound
        {
            return Err(crate::Error::PaymentConflict(
                "outbound merchant evidence transition is invalid".to_owned(),
            ));
        }
        Ok(())
    }
    pub(crate) fn outbound_evidence_replay_matches(event: &PaymentEvent, intent_id: &str, evidence_hash: &str) -> bool {
        event.intent_id == intent_id
            && event.event_type == crate::model::PaymentEventType::Submitted
            && event.attempt_no == 0
            && event.signature_ref.is_none()
            && event.tx_hash.is_none()
            && event.payload.get("evidence_hash").and_then(serde_json::Value::as_str) == Some(evidence_hash)
    }
}

impl PaymentQuery {
    pub(crate) fn entity_query(&self) -> EntityQuery {
        let mut query = EntityQuery::new();
        if let Some(value) = &self.scenario_id {
            query = query.compare("scenario_id", EntityComparison::Equal, EntityValue::from(value));
        }
        if let Some(value) = &self.wallet_id {
            query = query.compare("wallet_id", EntityComparison::Equal, EntityValue::from(value));
        }
        if let Some(value) = &self.run_id {
            query = query.compare("run_id", EntityComparison::Equal, EntityValue::from(value));
        }
        if let Some(value) = &self.direction {
            query = query.compare("direction", EntityComparison::Equal, EntityValue::from(value));
        }
        if let Some(value) = &self.status {
            query = query.compare("status", EntityComparison::Equal, EntityValue::from(value));
        }
        if let Some(value) = &self.from_created_at {
            query = query.compare("created_at", EntityComparison::GreaterOrEqual, EntityValue::from(value));
        }
        if let Some(value) = &self.to_created_at {
            query = query.compare("created_at", EntityComparison::LessOrEqual, EntityValue::from(value));
        }
        query
    }
}

#[cfg(test)]
#[path = "../../../../tests/storage/payment/mod_test.rs"]
mod tests;

#[path = "macro/rules.rs"]
mod validation;
pub(crate) use validation::*;

// Payment authority shared across SQL providers.

#[path = "macro/authority.rs"]
mod authority;
pub(crate) use authority::payment_authority;

#[cfg(test)]
#[path = "../../../../tests/storage/payment/authority_test.rs"]
mod authority_tests;
