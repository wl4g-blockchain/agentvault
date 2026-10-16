use std::sync::Arc;

use serde_json::json;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::{
    model::{
        canonicalize_payment_recipient, Amount, ApprovalRequest, ApprovalStatus, PaymentAuthorizationResult,
        PaymentChallenge, PaymentDirection, PaymentEvent, PaymentEventType, PaymentIntent, PaymentStatus,
        ReservationStatus,
    },
    storage::{
        AuthorizationAction, AuthorizationGuard, AuthorizationRecord, AuthorizationWrite, BudgetSnapshot,
        IdempotentWrite, OutboxMessage, OutboxStatus, PageRequest, PaymentStorage, PolicyStorage,
    },
    Error, Result,
};

use super::common::validate_identifier;

use super::{
    common::{canonical_http_origin, utc_day_start},
    AccessContext, PolicyDecision, PolicyEngine, ProtocolRegistry, ScenarioHandler, WalletBudget,
};

#[derive(Clone, Debug)]
pub struct AuthorizePayment {
    pub scenario_id: String,
    pub run_id: String,
    pub wallet_id: String,
    pub idempotency_key: String,
    pub challenge: PaymentChallenge,
}

/// Outbound policy, approval, idempotency, and budget orchestration. Storage
/// repeats aggregate budget checks in its authorization transaction.
pub struct PaymentHandler {
    scenarios: Arc<ScenarioHandler>,
    policies: Arc<dyn PolicyStorage>,
    payments: Arc<dyn PaymentStorage>,
    protocols: Arc<ProtocolRegistry>,
    approval_ttl_seconds: i64,
}

impl PaymentHandler {
    /// Creates an outbound handler.
    ///
    /// # Errors
    ///
    /// Returns an error when the approval TTL is not positive.
    pub fn new(
        scenarios: Arc<ScenarioHandler>,
        policies: Arc<dyn PolicyStorage>,
        payments: Arc<dyn PaymentStorage>,
        protocols: Arc<ProtocolRegistry>,
        approval_ttl_seconds: i64,
    ) -> Result<Self> {
        if approval_ttl_seconds <= 0 {
            return Err(Error::Config("approval TTL must be positive".to_owned()));
        }
        Ok(Self {
            scenarios,
            policies,
            payments,
            protocols,
            approval_ttl_seconds,
        })
    }

    /// Authorizes one outbound payment under scenario, policy, and shared-wallet
    /// budget constraints.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid or conflicting idempotency data, denied
    /// scenario access, missing policy, invalid protocol bindings, exhausted
    /// transactional budgets, or persistence failures.
    pub async fn authorize(
        &self,
        principal: &str,
        command: &AuthorizePayment,
        now: i64,
    ) -> Result<PaymentAuthorizationResult> {
        validate_command(command)?;
        let access = self.scenarios.authorize(principal, &command.scenario_id).await?;
        let request_hash = request_hash(command)?;
        if let Some(result) = self.replay_result(command, &request_hash).await? {
            return Ok(result);
        }

        let (policy, budget, origin, decision) = self.policy_decision(command, now).await?;
        let (intent, action) = build_payment_intent(command, &policy.id, &request_hash, &decision, now)?;
        self.protocols
            .get(&intent.protocol)?
            .validate_payment(&command.challenge, &intent)
            .await?;

        let artifacts = build_authorization_artifacts(
            &access,
            command,
            &intent,
            &decision,
            &request_hash,
            self.approval_ttl_seconds,
            now,
        );
        let guard = AuthorizationGuard {
            // In open-edge mode `ScenarioHandler` resolves the reserved
            // transport marker to the scenario owner's durable principal.
            // Persist that effective principal so the transaction can still
            // revalidate Client -> Scenario ownership under lock.
            principal: access.principal.clone(),
            client: access.client.clone(),
            scenario: access.scenario.clone(),
            policy,
            expected_wallet_updated_at: budget.wallet_updated_at,
            expected_wallet_key_version: budget.wallet_active_key_version,
            origin,
            day_started_at: utc_day_start(now),
            evaluated_at: now,
        };
        let write = AuthorizationWrite {
            intent: &intent,
            approval: artifacts.approval.as_ref(),
            action,
            guard: &guard,
            events: &artifacts.events,
            outbox: &artifacts.outbox,
        };
        let record = match self.payments.authorize(write).await? {
            IdempotentWrite::Inserted(record) | IdempotentWrite::Replay(record) => record,
        };
        if record.intent.request_hash != intent.request_hash {
            return Err(Error::PaymentConflict(
                "idempotency key was reused for different payment content".to_owned(),
            ));
        }
        authorization_result(&record)
    }

    async fn replay_result(
        &self,
        command: &AuthorizePayment,
        request_hash: &str,
    ) -> Result<Option<PaymentAuthorizationResult>> {
        let Some(existing) = self
            .payments
            .find_idempotency(&command.scenario_id, &command.idempotency_key)
            .await?
        else {
            return Ok(None);
        };
        if existing.intent.request_hash != request_hash {
            return Err(Error::PaymentConflict(
                "idempotency key was reused for different payment content".to_owned(),
            ));
        }
        authorization_result(&existing).map(Some)
    }

    async fn policy_decision(
        &self,
        command: &AuthorizePayment,
        now: i64,
    ) -> Result<(crate::model::PaymentPolicy, BudgetSnapshot, String, PolicyDecision)> {
        let policy = self
            .policies
            .list_for_scenario(
                &command.scenario_id,
                PageRequest {
                    offset: 0,
                    limit: 1_000,
                },
            )
            .await?
            .items
            .into_iter()
            .find(|policy| {
                policy.enabled
                    && policy.allows_direction(PaymentDirection::Outbound)
                    && policy.allows_wallet(&command.wallet_id)
            })
            .ok_or_else(|| Error::PolicyDenied("scenario has no enabled policy for this wallet".to_owned()))?;
        let budget_unit = command.challenge.asset.budget_unit();
        let budget = self
            .payments
            .budget_snapshot(
                &command.scenario_id,
                &command.wallet_id,
                PaymentDirection::Outbound,
                &budget_unit,
                utc_day_start(now),
                now,
            )
            .await?;
        if !budget.wallet_enabled {
            return Ok((
                policy,
                budget,
                canonical_http_origin(&command.challenge.resource)?,
                PolicyDecision::Denied("payment wallet is disabled".to_owned()),
            ));
        }
        if budget.wallet_active_key_version.is_none() {
            return Ok((
                policy,
                budget,
                canonical_http_origin(&command.challenge.resource)?,
                PolicyDecision::Denied("payment wallet has no active local signing key".to_owned()),
            ));
        }
        let origin = canonical_http_origin(&command.challenge.resource)?;
        let decision = PolicyEngine::evaluate(
            &policy,
            super::PolicyEvaluation {
                direction: PaymentDirection::Outbound,
                challenge: &command.challenge,
                wallet_id: &command.wallet_id,
                origin: &origin,
                scenario_usage: budget.scenario_spent_and_reserved,
                wallet_budget: WalletBudget {
                    usage: budget.wallet_spent_and_reserved,
                    shared_across_scenarios: budget.wallet_shared_across_scenarios,
                    max_per_payment: budget.wallet_max_per_payment,
                    max_per_day: budget.wallet_max_per_day,
                },
                now,
            },
        )?;
        Ok((policy, budget, origin, decision))
    }
}

struct AuthorizationArtifacts {
    approval: Option<ApprovalRequest>,
    events: Vec<PaymentEvent>,
    outbox: Vec<OutboxMessage>,
}

fn build_payment_intent(
    command: &AuthorizePayment,
    policy_id: &str,
    request_hash: &str,
    decision: &PolicyDecision,
    now: i64,
) -> Result<(PaymentIntent, AuthorizationAction)> {
    let (status, reservation_status, reserved_amount, action) = authorization_state(decision, command.challenge.amount);
    let intent = PaymentIntent {
        id: Uuid::new_v4().to_string(),
        scenario_id: command.scenario_id.clone(),
        run_id: command.run_id.clone(),
        wallet_id: Some(command.wallet_id.clone()),
        policy_id: Some(policy_id.to_owned()),
        offer_id: None,
        direction: PaymentDirection::Outbound,
        protocol: command.challenge.protocol.clone(),
        asset: command.challenge.asset.clone(),
        recipient: command.challenge.recipient.clone(),
        amount: command.challenge.amount,
        settled_amount: Amount::ZERO,
        resource: command.challenge.resource.clone(),
        challenge_hash: Some(command.challenge.challenge_hash.clone()),
        idempotency_key: command.idempotency_key.clone(),
        request_hash: request_hash.to_owned(),
        status,
        reservation_status,
        reserved_amount,
        tx_hash: None,
        settlement_ref: None,
        next_reconcile_at: None,
        created_at: now,
        updated_at: now,
        expires_at: Some(command.challenge.expires_at),
        settled_at: None,
    };
    intent
        .validate()
        .map_err(|field| Error::InvalidRequest(format!("invalid payment intent field: {field}")))?;
    Ok((intent, action))
}

fn authorization_state(
    decision: &PolicyDecision,
    amount: Amount,
) -> (PaymentStatus, ReservationStatus, Amount, AuthorizationAction) {
    match decision {
        PolicyDecision::Allowed => (
            PaymentStatus::Authorized,
            ReservationStatus::Reserved,
            amount,
            AuthorizationAction::Reserve,
        ),
        PolicyDecision::RequiresApproval => (
            PaymentStatus::PendingApproval,
            ReservationStatus::Unreserved,
            Amount::ZERO,
            AuthorizationAction::RequestApproval,
        ),
        PolicyDecision::Denied(_) => (
            PaymentStatus::Failed,
            ReservationStatus::Unreserved,
            Amount::ZERO,
            AuthorizationAction::RecordDenied,
        ),
    }
}

fn build_authorization_artifacts(
    access: &AccessContext,
    command: &AuthorizePayment,
    intent: &PaymentIntent,
    decision: &PolicyDecision,
    request_hash: &str,
    approval_ttl_seconds: i64,
    now: i64,
) -> AuthorizationArtifacts {
    let mut events = vec![payment_event(
        intent,
        PaymentEventType::ChallengeReceived,
        json!({
            "challenge_hash": command.challenge.challenge_hash,
            "challenge": command.challenge,
        }),
        now,
    )];
    let mut approval = None;
    let mut outbox = Vec::new();
    match decision {
        PolicyDecision::RequiresApproval => {
            let request = build_approval_request(access, command, intent, request_hash, approval_ttl_seconds, now);
            events.push(payment_event(
                intent,
                PaymentEventType::ApprovalRequested,
                json!({"approval_id": request.id, "amount": request.requested_amount}),
                now,
            ));
            outbox.push(approval_required_message(access, command, intent, &request, now));
            approval = Some(request);
        }
        PolicyDecision::Denied(reason) => events.push(payment_event(
            intent,
            PaymentEventType::Failed,
            json!({"reason": reason}),
            now,
        )),
        PolicyDecision::Allowed => {}
    }
    AuthorizationArtifacts {
        approval,
        events,
        outbox,
    }
}

fn build_approval_request(
    access: &AccessContext,
    command: &AuthorizePayment,
    intent: &PaymentIntent,
    request_hash: &str,
    approval_ttl_seconds: i64,
    now: i64,
) -> ApprovalRequest {
    ApprovalRequest {
        id: Uuid::new_v4().to_string(),
        intent_id: intent.id.clone(),
        requested_amount: intent.amount,
        approved_amount: None,
        requested_by: access.principal.clone(),
        approved_by: None,
        status: ApprovalStatus::Pending,
        intent_hash: request_hash.to_owned(),
        grant_jti: None,
        grant_claims_hash: None,
        sealed_grant_token: None,
        expires_at: command
            .challenge
            .expires_at
            .min(now.saturating_add(approval_ttl_seconds)),
        consumed_at: None,
        created_at: now,
        approved_at: None,
    }
}

fn approval_required_message(
    access: &AccessContext,
    command: &AuthorizePayment,
    intent: &PaymentIntent,
    approval: &ApprovalRequest,
    now: i64,
) -> OutboxMessage {
    OutboxMessage {
        id: Uuid::new_v4().to_string(),
        aggregate_type: "payment_intent".to_owned(),
        aggregate_id: intent.id.clone(),
        event_type: "approval_required".to_owned(),
        destination: format!("client:{}.approvals", access.client.id),
        correlation_id: intent.id.clone(),
        idempotency_key: format!("approval-required:{}", approval.id),
        payload: json!({
            "approval_id": approval.id,
            "intent_id": intent.id,
            "scenario_id": command.scenario_id,
            "amount": approval.requested_amount,
            "expires_at": approval.expires_at,
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

fn payment_event(
    intent: &PaymentIntent,
    event_type: PaymentEventType,
    payload: serde_json::Value,
    now: i64,
) -> PaymentEvent {
    PaymentEvent {
        id: Uuid::new_v4().to_string(),
        intent_id: intent.id.clone(),
        event_type,
        attempt_no: 0,
        signature_ref: None,
        tx_hash: None,
        payload,
        created_at: now,
    }
}

fn authorization_result(record: &AuthorizationRecord) -> Result<PaymentAuthorizationResult> {
    match record.intent.status {
        PaymentStatus::PendingApproval => {
            let approval = record
                .approval
                .as_ref()
                .ok_or_else(|| Error::Storage("pending intent has no approval row".to_owned()))?;
            Ok(PaymentAuthorizationResult::RequiresApproval {
                intent_id: record.intent.id.clone(),
                approval_id: approval.id.clone(),
            })
        }
        PaymentStatus::Failed | PaymentStatus::Expired => Ok(PaymentAuthorizationResult::Denied {
            intent_id: record.intent.id.clone(),
            reason: "payment authorization was denied or expired".to_owned(),
        }),
        _ => Ok(PaymentAuthorizationResult::Authorized {
            intent_id: record.intent.id.clone(),
            reserved_amount: record.intent.reserved_amount,
        }),
    }
}

fn validate_command(command: &AuthorizePayment) -> Result<()> {
    validate_identifier("scenario_id", &command.scenario_id)?;
    validate_identifier("run_id", &command.run_id)?;
    validate_identifier("wallet_id", &command.wallet_id)?;
    validate_identifier("idempotency_key", &command.idempotency_key)?;
    if command.challenge.scenario_id != command.scenario_id || command.challenge.offer_id.is_some() {
        return Err(Error::InvalidRequest(
            "outbound challenge scenario binding is invalid".to_owned(),
        ));
    }
    let canonical_recipient =
        canonicalize_payment_recipient(&command.challenge.asset.network, &command.challenge.recipient)
            .map_err(|_| Error::InvalidRequest("payment recipient is invalid for its network".to_owned()))?;
    if canonical_recipient != command.challenge.recipient {
        return Err(Error::InvalidRequest(
            "payment recipient must use its canonical network form".to_owned(),
        ));
    }
    Ok(())
}

fn request_hash(command: &AuthorizePayment) -> Result<String> {
    let encoded = serde_json::to_vec(&json!({
        "scenario_id": command.scenario_id,
        "run_id": command.run_id,
        "wallet_id": command.wallet_id,
        "challenge": command.challenge,
    }))?;
    let mut hash = Sha256::new();
    hash.update(b"agentvault.authorize-payment.v1");
    hash.update(u64::try_from(encoded.len()).unwrap_or(u64::MAX).to_be_bytes());
    hash.update(encoded);
    Ok(hex::encode(hash.finalize()))
}

#[cfg(test)]
#[path = "../../tests/handler/payment_handler_test.rs"]
mod tests;
