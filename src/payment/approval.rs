use super::{approval_grant::ApprovalGrantTokenService, validation::PaymentValidation};
use crate::{
    model::Amount, model::ApprovalDecision, model::ApprovalDecisionKind, model::ApprovalDecisionResult,
    model::ApprovalRequest, model::ApprovalStatus, model::PaymentEvent, model::PaymentEventType, model::PaymentIntent,
    model::PaymentStatus, model::ReservationStatus, model::SubmitApprovalDecision, storage::ApprovalConsumption,
    storage::ApprovalIntentTransition, storage::ApprovalReservationGuard, storage::ApprovalWrite,
    storage::IApprovalRepository, storage::IApproverRepository, storage::IIntentRepository, storage::OutboxMessage,
    storage::OutboxStatus, Error, Result,
};
use async_trait::async_trait;
use serde_json::json;
use std::sync::Arc;
use uuid::Uuid;

#[async_trait]
pub trait ApprovalPrincipalAuthorizer: Send + Sync {
    async fn authorize_approver(&self, principal: &str, scenario_id: &str) -> Result<()>;
}

/// Durable scenario-admin authorization backed by the same SQL authority as
/// payment ownership. No transport credential implicitly grants approval
/// rights, and disabled Client/Scenario rows fail closed inside the store.
pub struct StoredApprovalPrincipalAuthorizer {
    approvers: Arc<dyn IApproverRepository>,
}

impl StoredApprovalPrincipalAuthorizer {
    #[must_use]
    pub fn new(approvers: Arc<dyn IApproverRepository>) -> Self {
        Self { approvers }
    }
}

#[async_trait]
impl ApprovalPrincipalAuthorizer for StoredApprovalPrincipalAuthorizer {
    async fn authorize_approver(&self, principal: &str, scenario_id: &str) -> Result<()> {
        if principal == crate::model::scenario::UNRESTRICTED_PRINCIPAL {
            return Ok(());
        }
        if principal.trim().is_empty() || scenario_id.trim().is_empty() {
            return Err(Error::Forbidden);
        }
        if self.approvers.is_authorized(scenario_id, principal).await? {
            Ok(())
        } else {
            Err(Error::Forbidden)
        }
    }
}

pub struct ApprovalWorkflow {
    approvals: Arc<dyn IApprovalRepository>,
    payments: Arc<dyn IIntentRepository>,
    authorizer: Arc<dyn ApprovalPrincipalAuthorizer>,
    grant_tokens: Arc<dyn ApprovalGrantTokenService>,
    grant_ttl_seconds: i64,
}

impl ApprovalWorkflow {
    /// Creates an approval handler with a bounded one-time grant lifetime.
    ///
    /// # Errors
    ///
    /// Returns an error when the grant TTL is not positive.
    pub fn new(
        approvals: Arc<dyn IApprovalRepository>,
        payments: Arc<dyn IIntentRepository>,
        authorizer: Arc<dyn ApprovalPrincipalAuthorizer>,
        grant_tokens: Arc<dyn ApprovalGrantTokenService>,
        grant_ttl_seconds: i64,
    ) -> Result<Self> {
        if grant_ttl_seconds <= 0 {
            return Err(Error::Config("approval grant TTL must be positive".to_owned()));
        }
        Ok(Self {
            approvals,
            payments,
            authorizer,
            grant_tokens,
            grant_ttl_seconds,
        })
    }

    /// Applies an authenticated administrator's decision to a pending approval.
    /// Exact retries verify and reuse the JWT already sealed in the database.
    ///
    /// # Errors
    ///
    /// Returns an error when the approval or intent does not exist, their
    /// bindings or lifecycle state are invalid, the principal is not allowed to
    /// approve the scenario, token protection fails, or the atomic write fails.
    pub async fn submit_decision(
        &self,
        principal: &str,
        request: &SubmitApprovalDecision,
        now: i64,
    ) -> Result<ApprovalDecisionResult> {
        if let Some(replay) = self.replay_decision(principal, request, now).await? {
            return Ok(replay);
        }
        let (approval, intent, decision) = self.decision_context(principal, request, now).await?;
        let grant = if decision.decision == ApprovalDecisionKind::Approve {
            let expires_at = ApprovalWorkflow::grant_expiration(approval.expires_at, now, self.grant_ttl_seconds);
            Some(self.grant_tokens.issue(&approval, &decision, expires_at)?)
        } else {
            None
        };
        let granted = grant.is_some();
        let mut rejected_intent = None;
        if !granted {
            let mut failed = intent.clone();
            failed.status = PaymentStatus::Failed;
            failed.reservation_status = ReservationStatus::Unreserved;
            failed.reserved_amount = Amount::ZERO;
            failed.next_reconcile_at = None;
            failed.updated_at = ApprovalWorkflow::next_timestamp(intent.updated_at, now);
            rejected_intent = Some(failed);
        }
        let event = ApprovalWorkflow::approval_decision_event(&approval, &intent, principal, granted, now);
        let outbox = vec![ApprovalWorkflow::approval_decision_message(
            &approval, &intent, granted, now,
        )];
        let persisted = self
            .approvals
            .decide(ApprovalWrite {
                decision: &decision,
                grant: grant.as_ref(),
                intent_transition: rejected_intent.as_ref().map(|failed| ApprovalIntentTransition {
                    intent: failed,
                    expected_updated_at: intent.updated_at,
                }),
                event: &event,
                outbox: &outbox,
            })
            .await?;
        Ok(ApprovalDecisionResult { approval: persisted })
    }

    async fn replay_decision(
        &self,
        principal: &str,
        request: &SubmitApprovalDecision,
        now: i64,
    ) -> Result<Option<ApprovalDecisionResult>> {
        let approval = self
            .approvals
            .get(&request.approval_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment approval".to_owned()))?;
        if approval.status == ApprovalStatus::Pending {
            return Ok(None);
        }
        if approval.intent_id != request.intent_id {
            return Err(Error::PaymentConflict("approval is bound to another intent".to_owned()));
        }
        let intent = self
            .payments
            .get(&request.intent_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment intent".to_owned()))?;
        self.authorizer
            .authorize_approver(principal, &intent.scenario_id)
            .await?;
        if approval.requested_by == principal || approval.approved_by.as_deref() != Some(principal) {
            return Err(Error::Forbidden);
        }

        match (approval.status, request.decision) {
            (ApprovalStatus::Approved, ApprovalDecisionKind::Approve) => {
                if request.approved_amount != approval.approved_amount {
                    return Err(Error::PaymentConflict("approval decision replay changed".to_owned()));
                }
                let verified = self.grant_tokens.verify(&approval, now)?;
                if verified.claims.intent_id != intent.id
                    || verified.claims.intent_hash != intent.request_hash
                    || verified.claims.amount != intent.amount
                {
                    return Err(Error::PaymentConflict("approval decision replay changed".to_owned()));
                }
                Ok(Some(ApprovalDecisionResult { approval }))
            }
            (ApprovalStatus::Consumed, ApprovalDecisionKind::Approve)
                if request.approved_amount == approval.approved_amount =>
            {
                // Consumption is the terminal proof that the persisted grant
                // was already authenticated and reserved exactly once. An
                // exact administrator retry must remain recoverable after the
                // short-lived JWT expires and must never consume it again.
                Ok(Some(ApprovalDecisionResult { approval }))
            }
            (ApprovalStatus::Rejected, ApprovalDecisionKind::Reject) if request.approved_amount.is_none() => {
                Ok(Some(ApprovalDecisionResult { approval }))
            }
            _ => Err(Error::PaymentConflict("approval decision replay changed".to_owned())),
        }
    }

    async fn decision_context(
        &self,
        principal: &str,
        request: &SubmitApprovalDecision,
        now: i64,
    ) -> Result<(ApprovalRequest, PaymentIntent, ApprovalDecision)> {
        let approval = self
            .approvals
            .get(&request.approval_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment approval".to_owned()))?;
        let intent = self
            .payments
            .get(&request.intent_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment intent".to_owned()))?;
        if approval.intent_id != intent.id
            || approval.intent_id != request.intent_id
            || approval.intent_hash != intent.request_hash
        {
            return Err(Error::PaymentConflict("approval is bound to another intent".to_owned()));
        }
        self.authorizer
            .authorize_approver(principal, &intent.scenario_id)
            .await?;
        if approval.requested_by == principal {
            return Err(Error::Forbidden);
        }
        if request.decision == ApprovalDecisionKind::Approve && request.approved_amount != Some(intent.amount) {
            return Err(Error::InvalidRequest(
                "fixed-charge approval must authorize the exact intent amount".to_owned(),
            ));
        }
        let decision = ApprovalWorkflow::approval_decision_from_request(principal, request, now);
        approval
            .validate_decision(&decision, now)
            .map_err(|field| Error::InvalidTransition(format!("invalid approval decision: {field}")))?;
        Ok((approval, intent, decision))
    }

    /// Verifies and atomically consumes the internally stored one-time JWT,
    /// then reserves the bound payment budget.
    ///
    /// # Errors
    ///
    /// Returns an error when the approval or intent is missing, the principal
    /// is unauthorized, the sealed JWT is invalid or expired, the grant was
    /// already consumed, or the authoritative reservation transaction fails.
    pub async fn consume(
        &self,
        principal: &str,
        approval_id: &str,
        intent_id: &str,
        now: i64,
    ) -> Result<PaymentIntent> {
        let approval = self
            .approvals
            .get(approval_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment approval".to_owned()))?;
        let mut intent = self
            .payments
            .get(intent_id)
            .await?
            .ok_or_else(|| Error::NotFound("payment intent".to_owned()))?;
        self.authorizer
            .authorize_approver(principal, &intent.scenario_id)
            .await?;
        if approval.intent_id != intent.id
            || approval.intent_hash != intent.request_hash
            || approval.status != ApprovalStatus::Approved
            || intent.status != PaymentStatus::PendingApproval
        {
            return Err(Error::InvalidTransition(
                "approval grant is not available for this intent".to_owned(),
            ));
        }
        let grant = self.grant_tokens.verify(&approval, now)?;
        if grant.claims.intent_id != intent.id
            || grant.claims.intent_hash != intent.request_hash
            || grant.claims.amount != intent.amount
        {
            return Err(Error::Forbidden);
        }
        let expected_updated_at = intent.updated_at;
        intent.status = PaymentStatus::Authorized;
        intent.reservation_status = ReservationStatus::Reserved;
        intent.reserved_amount = grant.claims.amount;
        intent.updated_at = ApprovalWorkflow::next_timestamp(intent.updated_at, now);
        let event = PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type: PaymentEventType::ApprovalConsumed,
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload: json!({"approval_id": approval.id}),
            created_at: now,
        };
        let outbox = vec![OutboxMessage {
            id: Uuid::new_v4().to_string(),
            aggregate_type: "payment_intent".to_owned(),
            aggregate_id: intent.id.clone(),
            event_type: "payment_authorized".to_owned(),
            destination: format!("scenario:{}.payments", intent.scenario_id),
            run_id: intent.run_id.clone(),
            payload: json!({"approval_id": approval.id, "intent_id": intent.id, "run_id": intent.run_id}),
            status: OutboxStatus::Pending,
            attempt_count: 0,
            available_at: now,
            lease_owner: None,
            lease_expires_at: None,
            last_error: None,
            created_at: now,
            delivered_at: None,
        }];
        let origin = PaymentValidation::canonical_http_origin(&intent.resource)?;
        self.approvals
            .consume_and_reserve(ApprovalConsumption {
                grant: &grant,
                intent: &intent,
                guard: ApprovalReservationGuard {
                    origin: &origin,
                    day_started_at: PaymentValidation::utc_day_start(now),
                    evaluated_at: now,
                },
                expected_intent_updated_at: expected_updated_at,
                consumed_at: now,
                event: &event,
                outbox: &outbox,
            })
            .await
    }
}

impl ApprovalWorkflow {
    pub(crate) fn approval_decision_from_request(
        principal: &str,
        request: &SubmitApprovalDecision,
        now: i64,
    ) -> ApprovalDecision {
        ApprovalDecision {
            approval_id: request.approval_id.clone(),
            intent_id: request.intent_id.clone(),
            decision: request.decision,
            approved_amount: request.approved_amount,
            approved_by: principal.to_owned(),
            reason: request.reason.clone(),
            decided_at: now,
        }
    }

    pub(crate) fn next_timestamp(previous: i64, now: i64) -> i64 {
        now.max(previous.saturating_add(1))
    }

    pub(crate) fn grant_expiration(request_expires_at: i64, decided_at: i64, grant_ttl_seconds: i64) -> i64 {
        request_expires_at.min(decided_at.saturating_add(grant_ttl_seconds))
    }

    pub(crate) fn approval_decision_event(
        approval: &ApprovalRequest,
        intent: &PaymentIntent,
        principal: &str,
        granted: bool,
        now: i64,
    ) -> PaymentEvent {
        PaymentEvent {
            id: Uuid::new_v4().to_string(),
            intent_id: intent.id.clone(),
            event_type: if granted {
                PaymentEventType::ApprovalGranted
            } else {
                PaymentEventType::ApprovalRejected
            },
            attempt_no: 0,
            signature_ref: None,
            tx_hash: None,
            payload: json!({"approval_id": approval.id, "approved_by": principal}),
            created_at: now,
        }
    }

    pub(crate) fn approval_decision_message(
        approval: &ApprovalRequest,
        intent: &PaymentIntent,
        granted: bool,
        now: i64,
    ) -> OutboxMessage {
        OutboxMessage {
            id: Uuid::new_v4().to_string(),
            aggregate_type: "payment_approval".to_owned(),
            aggregate_id: approval.id.clone(),
            event_type: if granted {
                "approval_granted".to_owned()
            } else {
                "approval_rejected".to_owned()
            },
            destination: format!("scenario:{}.payments", intent.scenario_id),
            run_id: intent.run_id.clone(),
            payload: json!({"approval_id": approval.id, "intent_id": intent.id, "run_id": intent.run_id}),
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
}

#[cfg(test)]
#[path = "../../tests/payment/approval_test.rs"]
mod tests;
