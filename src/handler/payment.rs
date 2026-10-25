use std::sync::Arc;

use serde_json::json;
use sha2::{Digest as _, Sha256};
use uuid::Uuid;

use crate::{
    apm::AgentVaultMetrics,
    model::{
        canonicalize_payment_recipient, wallet::is_evm_payment_network, wallet::is_evm_wallet_algorithm,
        wallet::WalletSelectionStrategy, Amount, ApprovalRequest, ApprovalStatus, AuthorizePayment,
        PaymentAuthorizationResult, PaymentDirection, PaymentEvent, PaymentEventType, PaymentIntent, PaymentPolicy,
        PaymentStatus, PolicyDecision, PolicyEngine, PolicyEvaluation, ReservationStatus, WalletBudget,
    },
    storage::{
        AuthorizationAction, AuthorizationGuard, AuthorizationRecord, AuthorizationWrite, BudgetSnapshot,
        IdempotentWrite, OutboxMessage, OutboxStatus, PageRequest, PaymentStorage, PolicyStorage,
    },
    Error, Result,
};

use super::payment_validation::validate_identifier;

use super::{
    payment_validation::{canonical_http_origin, utc_day_start},
    AccessContext, ProtocolRegistry, ScenarioHandler,
};

const MAX_SCENARIO_POLICIES: u32 = 1_000;
const MAX_WALLET_CANDIDATES: usize = 4_096;

/// Outbound policy, approval, run replay, and budget orchestration. Storage
/// repeats aggregate budget checks in its authorization transaction.
pub struct PaymentHandler {
    scenarios: Arc<ScenarioHandler>,
    policies: Arc<dyn PolicyStorage>,
    payments: Arc<dyn PaymentStorage>,
    protocols: Arc<ProtocolRegistry>,
    approval_ttl_seconds: i64,
    wallet_selection_strategy: WalletSelectionStrategy,
    metrics: AgentVaultMetrics,
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
        wallet_selection_strategy: WalletSelectionStrategy,
        metrics: AgentVaultMetrics,
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
            wallet_selection_strategy,
            metrics,
        })
    }

    /// Authorizes one outbound payment under scenario, policy, and shared-wallet
    /// budget constraints.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or conflicting run identity, denied
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

        let selected = self.policy_decision(command, &request_hash, now).await?;
        tracing::info!(
            event.name = "agentvault.wallet.selected",
            run.id = %command.run_id,
            scenario.id = %command.scenario_id,
            policy.id = %selected.policy.id,
            wallet.id = %selected.wallet_id,
            wallet.selection_strategy = ?self.wallet_selection_strategy,
            "selected outbound wallet from the scenario policy pool"
        );
        self.metrics
            .record_wallet_selection(self.wallet_selection_strategy.as_str());
        let (intent, action) = build_payment_intent(
            command,
            &selected.policy.id,
            &selected.wallet_id,
            &request_hash,
            &selected.decision,
            now,
        )?;
        self.protocols
            .get(&intent.protocol)?
            .validate_payment(&command.challenge, &intent)
            .await?;

        let artifacts = build_authorization_artifacts(
            &access,
            command,
            &intent,
            &selected.decision,
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
            policy: selected.policy,
            expected_wallet_updated_at: selected.budget.wallet_updated_at,
            expected_wallet_key_version: selected.budget.wallet_active_key_version,
            origin: selected.origin,
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
                "run_id was reused for different payment content".to_owned(),
            ));
        }
        authorization_result(&record)
    }

    async fn replay_result(
        &self,
        command: &AuthorizePayment,
        request_hash: &str,
    ) -> Result<Option<PaymentAuthorizationResult>> {
        let Some(existing) = self.payments.find_run(&command.scenario_id, &command.run_id).await? else {
            return Ok(None);
        };
        if existing.intent.request_hash != request_hash {
            return Err(Error::PaymentConflict(
                "run_id was reused for different payment content".to_owned(),
            ));
        }
        authorization_result(&existing).map(Some)
    }

    async fn policy_decision(
        &self,
        command: &AuthorizePayment,
        request_hash: &str,
        now: i64,
    ) -> Result<PolicyWalletCandidate> {
        let page = self
            .policies
            .list_for_scenario(
                &command.scenario_id,
                PageRequest {
                    offset: 0,
                    limit: MAX_SCENARIO_POLICIES,
                },
            )
            .await?;
        if page.total != u64::try_from(page.items.len()).unwrap_or(u64::MAX) {
            return Err(Error::PolicyDenied(format!(
                "scenario has more than {MAX_SCENARIO_POLICIES} policies; wallet selection is bounded"
            )));
        }

        let origin = canonical_http_origin(&command.challenge.resource)?;
        let budget_unit = command.challenge.asset.budget_unit();
        let mut candidates = Vec::new();
        for policy in page.items {
            if policy.enabled && policy.allows_direction(PaymentDirection::Outbound) {
                policy
                    .validate()
                    .map_err(|field| Error::PolicyDenied(format!("invalid policy field: {field}")))?;
            }
            if !policy.enabled
                || !policy.allows_direction(PaymentDirection::Outbound)
                || !policy.allows_protocol(&command.challenge.protocol)
                || !policy.allows_asset(&command.challenge.asset)
                || !policy.allows_payment_recipient(&command.challenge.asset.network, &command.challenge.recipient)
                || !policy.allows_origin(&origin)
            {
                continue;
            }
            for wallet_id in &policy.wallet_ids {
                if candidates.len() >= MAX_WALLET_CANDIDATES {
                    return Err(Error::PolicyDenied(format!(
                        "scenario has more than {MAX_WALLET_CANDIDATES} eligible policy-wallet candidates"
                    )));
                }
                let budget = self
                    .payments
                    .budget_snapshot(
                        &command.scenario_id,
                        wallet_id,
                        PaymentDirection::Outbound,
                        &budget_unit,
                        utc_day_start(now),
                        now,
                    )
                    .await?;
                let decision = if !budget.wallet_enabled {
                    PolicyDecision::Denied("payment wallet is disabled".to_owned())
                } else if budget.wallet_active_key_version.is_none() {
                    PolicyDecision::Denied("payment wallet has no active local signing key".to_owned())
                } else if !wallet_supports_payment_network(&budget, &command.challenge.asset.network) {
                    PolicyDecision::Denied("payment wallet is incompatible with the payment network".to_owned())
                } else {
                    PolicyEngine::evaluate(
                        &policy,
                        PolicyEvaluation {
                            direction: PaymentDirection::Outbound,
                            challenge: &command.challenge,
                            wallet_id,
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
                    )?
                };
                candidates.push(PolicyWalletCandidate {
                    policy: policy.clone(),
                    wallet_id: wallet_id.clone(),
                    selection_key: selection_key(request_hash, &policy.id, wallet_id),
                    budget,
                    origin: origin.clone(),
                    decision,
                });
            }
        }
        candidates.sort_by(|left, right| compare_candidates(self.wallet_selection_strategy, left, right));
        candidates
            .into_iter()
            .next()
            .ok_or_else(|| Error::PolicyDenied("scenario has no eligible outbound policy-wallet candidate".to_owned()))
    }
}

fn wallet_supports_payment_network(budget: &BudgetSnapshot, network: &str) -> bool {
    (!is_evm_payment_network(network) || is_evm_wallet_algorithm(&budget.wallet_algorithm))
        && canonicalize_payment_recipient(network, &budget.wallet_address).is_ok()
}

struct PolicyWalletCandidate {
    policy: PaymentPolicy,
    wallet_id: String,
    selection_key: [u8; 32],
    budget: BudgetSnapshot,
    origin: String,
    decision: PolicyDecision,
}

fn compare_candidates(
    strategy: WalletSelectionStrategy,
    left: &PolicyWalletCandidate,
    right: &PolicyWalletCandidate,
) -> std::cmp::Ordering {
    let ranked = decision_rank(&left.decision).cmp(&decision_rank(&right.decision));
    let ranked = match strategy {
        WalletSelectionStrategy::LeastSpent => ranked
            .then_with(|| {
                left.budget
                    .wallet_spent_and_reserved
                    .cmp(&right.budget.wallet_spent_and_reserved)
            })
            .then_with(|| left.selection_key.cmp(&right.selection_key)),
        WalletSelectionStrategy::StableRandom => ranked
            .then_with(|| left.selection_key.cmp(&right.selection_key))
            .then_with(|| {
                left.budget
                    .wallet_spent_and_reserved
                    .cmp(&right.budget.wallet_spent_and_reserved)
            }),
    };
    ranked
        .then_with(|| left.policy.id.cmp(&right.policy.id))
        .then_with(|| left.wallet_id.cmp(&right.wallet_id))
}

const fn decision_rank(decision: &PolicyDecision) -> u8 {
    match decision {
        PolicyDecision::Allowed => 0,
        PolicyDecision::RequiresApproval => 1,
        PolicyDecision::Denied(_) => 2,
    }
}

fn selection_key(request_hash: &str, policy_id: &str, wallet_id: &str) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"agentvault.wallet-selection.v1");
    for value in [request_hash, policy_id, wallet_id] {
        hash.update(u64::try_from(value.len()).unwrap_or(u64::MAX).to_be_bytes());
        hash.update(value.as_bytes());
    }
    hash.finalize().into()
}

struct AuthorizationArtifacts {
    approval: Option<ApprovalRequest>,
    events: Vec<PaymentEvent>,
    outbox: Vec<OutboxMessage>,
}

fn build_payment_intent(
    command: &AuthorizePayment,
    policy_id: &str,
    wallet_id: &str,
    request_hash: &str,
    decision: &PolicyDecision,
    now: i64,
) -> Result<(PaymentIntent, AuthorizationAction)> {
    let (status, reservation_status, reserved_amount, action) = authorization_state(decision, command.challenge.amount);
    let intent = PaymentIntent {
        id: Uuid::new_v4().to_string(),
        scenario_id: command.scenario_id.clone(),
        run_id: command.run_id.clone(),
        wallet_id: Some(wallet_id.to_owned()),
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
        run_id: intent.run_id.clone(),
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
        run_id: intent.run_id.clone(),
        payload: json!({
            "approval_id": approval.id,
            "intent_id": intent.id,
            "scenario_id": command.scenario_id,
            "run_id": intent.run_id,
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
        "challenge": command.challenge,
    }))?;
    Ok(hash_request(b"agentvault.authorize-payment.v2", &encoded))
}

fn hash_request(domain: &[u8], encoded: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(domain);
    hash.update(u64::try_from(encoded.len()).unwrap_or(u64::MAX).to_be_bytes());
    hash.update(encoded);
    hex::encode(hash.finalize())
}

#[cfg(test)]
#[path = "../../tests/handler/payment_test.rs"]
mod tests;
