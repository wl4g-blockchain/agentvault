//! Approval persistence contract.

use crate::{
    model::{ApprovalDecision, ApprovalGrant, ApprovalRequest, PaymentEvent, PaymentIntent, VerifiedApprovalGrant},
    storage::{IdempotentWrite, OutboxMessage, Page, PageRequest},
    Result,
};
use async_trait::async_trait;

pub mod postgres;
pub mod sqlite;
pub(crate) mod transactions;

/// Atomic approval decision. Only JWT metadata and its AEAD envelope cross
/// this boundary; the plaintext compact token is never persisted or returned.
pub struct ApprovalWrite<'a> {
    pub decision: &'a ApprovalDecision,
    pub grant: Option<&'a ApprovalGrant>,
    pub intent_transition: Option<ApprovalIntentTransition<'a>>,
    pub event: &'a PaymentEvent,
    pub outbox: &'a [OutboxMessage],
}

/// Compare-and-swap transition applied with a rejection decision.
#[derive(Clone, Copy, Debug)]
pub struct ApprovalIntentTransition<'a> {
    pub intent: &'a PaymentIntent,
    pub expected_updated_at: i64,
}

/// Atomic one-time grant consumption and budget reservation.
pub struct ApprovalConsumption<'a> {
    pub grant: &'a VerifiedApprovalGrant,
    pub intent: &'a PaymentIntent,
    pub guard: ApprovalReservationGuard<'a>,
    pub expected_intent_updated_at: i64,
    pub consumed_at: i64,
    pub event: &'a PaymentEvent,
    pub outbox: &'a [OutboxMessage],
}

/// Current request facts used to re-evaluate an approved payment inside the
/// authoritative reservation transaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApprovalReservationGuard<'a> {
    pub origin: &'a str,
    pub day_started_at: i64,
    pub evaluated_at: i64,
}

#[async_trait]
pub trait IApprovalRepository: Send + Sync {
    async fn request(&self, approval: &ApprovalRequest) -> Result<IdempotentWrite<ApprovalRequest>>;

    async fn get(&self, approval_id: &str) -> Result<Option<ApprovalRequest>>;

    async fn get_active_for_intent(&self, intent_id: &str) -> Result<Option<ApprovalRequest>>;

    async fn list_pending(&self, scenario_id: &str, now: i64, page: PageRequest) -> Result<Page<ApprovalRequest>>;

    /// Applies an authenticated administrator decision, records its event, and
    /// persists the JWT JTI, claims hash, sealed token, and notification in one transaction. The
    /// locked approval `intent_hash` must equal the locked intent
    /// `request_hash`; any mismatch is a conflict. Rejection
    /// must CAS the bound intent to `Failed` in that transaction. For an
    /// approval, the row's `expires_at` must become the grant's shorter expiry;
    /// the original pending-request deadline must never extend the configured
    /// grant lifetime.
    async fn decide(&self, write: ApprovalWrite<'_>) -> Result<ApprovalRequest>;

    /// Atomically consumes a matching, authenticated, unexpired JWT exactly once and
    /// reserves policy and shared-wallet budgets for the bound intent. The
    /// transaction must lock/reload the approval, intent, client, scenario,
    /// policy, wallet, and active key row; require
    /// `approval.intent_hash == intent.request_hash == grant.claims.intent_hash`,
    /// require the stored JTI and claims hash to match the verified grant,
    /// recheck enabled state, ownership,
    /// local custody, an active non-compromised signing key, complete policy
    /// scope, explicit policy-wallet membership, outbound direction, intent
    /// expiry, matching policy/wallet budget unit, and current hard limits.
    /// The approval may exceed the policy's automatic `max_per_day` only up to
    /// `max_per_day + approval_overage`; it can never bypass that ceiling or a
    /// wallet hard limit. If enabled policies from multiple scenarios share the
    /// wallet, both wallet-wide limits are mandatory. Both aggregates include
    /// every matching outbound `reserved` intent regardless of creation day,
    /// plus current-day confirmed outbound ledger facts selected by
    /// `effective_at`. Intent `created_at`, network, and token contract must
    /// not partition wallet usage. Only then may the
    /// grant be consumed and funds reserved. A lifecycle/policy change since
    /// approval fails closed.
    async fn consume_and_reserve(&self, consumption: ApprovalConsumption<'_>) -> Result<PaymentIntent>;

    /// Atomically expires due pending requests and approved, unconsumed grants.
    /// Each row's bound intent must transition to `Expired` with no reservation,
    /// and its expiry event/outbox notification must commit in the same
    /// transaction. Implementations use conditional updates so concurrent
    /// consumption or decisions win at most once.
    async fn expire_due(&self, now: i64, limit: u32) -> Result<Vec<ApprovalRequest>>;
}

#[cfg(test)]
#[path = "../../../tests/storage/approval/mod_test.rs"]
mod tests;

mod validation {
    use crate::{
        model::{ApprovalRequest, ApprovalStatus, PaymentEvent, PaymentEventType, PaymentStatus, ReservationStatus},
        storage::{OutboxMessage, OutboxStatus},
        Result,
    };
    use serde_json::json;

    pub(crate) struct ApprovalStorageRules;
    impl ApprovalStorageRules {
        pub(crate) fn expiry_event(
            approval: &ApprovalRequest,
            intent: &crate::model::PaymentIntent,
            now: i64,
        ) -> PaymentEvent {
            PaymentEvent {
                id: format!("approval-expired:{}", approval.id),
                intent_id: intent.id.clone(),
                event_type: PaymentEventType::Failed,
                attempt_no: 0,
                signature_ref: None,
                tx_hash: None,
                payload: json!({"reason": "approval_expired", "approval_id": approval.id}),
                created_at: now,
            }
        }
        pub(crate) fn expiry_outbox(
            approval: &ApprovalRequest,
            intent: &crate::model::PaymentIntent,
            now: i64,
        ) -> OutboxMessage {
            OutboxMessage {
                id: format!("approval-expired:{}", approval.id),
                aggregate_type: "payment_intent".to_owned(),
                aggregate_id: intent.id.clone(),
                event_type: "approval_expired".to_owned(),
                destination: format!("scenario:{}.payments", intent.scenario_id),
                run_id: intent.run_id.clone(),
                payload: json!({
                    "intent_id": intent.id,
                    "approval_id": approval.id,
                    "scenario_id": intent.scenario_id
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
        pub(crate) fn validate_grant(
            approval: &ApprovalRequest,
            intent: &crate::model::PaymentIntent,
            decision: &crate::model::ApprovalDecision,
            grant: &crate::model::ApprovalGrant,
        ) -> Result<()> {
            grant
                .claims
                .validate()
                .map_err(|field| crate::Error::conflict(format!("approval JWT claim {field} is invalid")))?;
            if grant.claims.approval_id != approval.id
                || grant.claims.intent_id != approval.intent_id
                || grant.claims.intent_hash != approval.intent_hash
                || grant.claims.intent_hash != intent.request_hash
                || Some(grant.claims.amount) != decision.approved_amount
                || grant.claims.approver != decision.approved_by
                || grant.claims.iat != decision.decided_at
                || grant.claims.exp > approval.expires_at
                || grant.claims_hash.len() != 64
            {
                return Err(crate::Error::conflict(
                    "approval JWT is not bound to the decision and intent",
                ));
            }
            Ok(())
        }
        pub(crate) fn validate_verified_grant(
            approval: &ApprovalRequest,
            intent: &crate::model::PaymentIntent,
            grant: &crate::model::VerifiedApprovalGrant,
            now: i64,
        ) -> Result<()> {
            if approval.status != ApprovalStatus::Approved
                || approval.expires_at <= now
                || grant.claims.exp <= now
                || grant.claims.approval_id != approval.id
                || grant.claims.intent_id != intent.id
                || grant.claims.intent_hash != approval.intent_hash
                || grant.claims.intent_hash != intent.request_hash
                || approval.grant_jti.as_deref() != Some(grant.claims.jti.as_str())
                || approval.grant_claims_hash.as_deref() != Some(grant.claims_hash.as_str())
                || approval.approved_amount != Some(grant.claims.amount)
                || approval.approved_by.as_deref() != Some(grant.claims.approver.as_str())
                || approval.requested_amount != intent.amount
                || grant.claims.amount != intent.amount
                || intent.direction != crate::model::PaymentDirection::Outbound
                || intent.status != PaymentStatus::Authorized
                || intent.reservation_status != ReservationStatus::Reserved
                || intent.reserved_amount != intent.amount
            {
                return Err(crate::Error::conflict(
                    "verified approval JWT does not match stored authorization state",
                ));
            }
            Ok(())
        }
    }
}
pub(crate) use validation::*;
