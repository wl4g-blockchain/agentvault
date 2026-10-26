use super::payment::{Amount, PaymentChallenge, PaymentDirection};
use crate::{Error, Result};

pub use super::domain::{PaymentPolicy, PaymentPolicyLimits};

/// Current aggregate usage and hard limits for one candidate Wallet.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct WalletBudget {
    /// Outstanding reservations plus today's confirmed same-direction flow.
    pub usage: Amount,
    /// Shared Wallets require explicit aggregate hard limits.
    pub shared_across_scenarios: bool,
    pub max_per_payment: Option<Amount>,
    pub max_per_day: Option<Amount>,
}

/// Result of deterministic policy evaluation before the authoritative SQL
/// reservation transaction repeats aggregate checks.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PolicyDecision {
    Allowed,
    RequiresApproval,
    Denied(String),
}

/// Complete direction-aware input to one deterministic policy evaluation.
#[derive(Clone, Copy, Debug)]
pub struct PolicyEvaluation<'a> {
    pub direction: PaymentDirection,
    pub challenge: &'a PaymentChallenge,
    pub wallet_id: &'a str,
    pub origin: &'a str,
    pub scenario_usage: Amount,
    pub wallet_budget: WalletBudget,
    pub now: i64,
}

/// Pure domain policy evaluator. Storage repeats authoritative aggregate
/// checks in the same transaction as reservation.
pub struct PolicyEngine;

impl PolicyEngine {
    /// Evaluates policy scope, directional budgets, shared-Wallet limits and
    /// approval headroom.
    ///
    /// # Errors
    ///
    /// Returns an error when the policy aggregate itself is invalid.
    pub fn evaluate(policy: &PaymentPolicy, evaluation: PolicyEvaluation<'_>) -> Result<PolicyDecision> {
        let PolicyEvaluation {
            direction,
            challenge,
            wallet_id,
            origin,
            scenario_usage,
            wallet_budget,
            now,
        } = evaluation;
        policy
            .validate()
            .map_err(|field| Error::PolicyDenied(format!("invalid policy field: {field}")))?;
        if challenge.expires_at <= now {
            return Ok(PolicyDecision::Denied("challenge expired".to_owned()));
        }
        let Some(limits) = policy.limits_for(direction) else {
            return Ok(PolicyDecision::Denied(
                "policy does not authorize this payment direction".to_owned(),
            ));
        };
        if !policy.allows_wallet(wallet_id)
            || policy.scenario_id != challenge.scenario_id
            || !policy.allows_protocol(&challenge.protocol)
            || !policy.allows_asset(&challenge.asset)
            || !policy.allows_payment_recipient(&challenge.asset.network, &challenge.recipient)
            || !policy.allows_origin(origin)
        {
            return Ok(PolicyDecision::Denied(
                "policy scope does not match the payment".to_owned(),
            ));
        }
        if challenge.amount > limits.max_per_payment {
            return Ok(PolicyDecision::Denied("per-payment policy limit exceeded".to_owned()));
        }
        let scenario_after = scenario_usage.checked_add(challenge.amount);
        let hard_daily_limit = limits
            .hard_daily_limit()
            .ok_or_else(|| Error::PolicyDenied("daily policy limit overflow".to_owned()))?;
        if scenario_after.is_none_or(|total| total > hard_daily_limit) {
            return Ok(PolicyDecision::Denied(
                "daily scenario hard ceiling exceeded".to_owned(),
            ));
        }
        if wallet_budget.shared_across_scenarios
            && (wallet_budget.max_per_payment.is_none() || wallet_budget.max_per_day.is_none())
        {
            return Ok(PolicyDecision::Denied(
                "shared-wallet hard limits are required".to_owned(),
            ));
        }
        if wallet_budget
            .max_per_payment
            .is_some_and(|limit| challenge.amount > limit)
            || wallet_budget
                .max_per_day
                .is_some_and(|limit| exceeds(wallet_budget.usage, challenge.amount, limit))
        {
            return Ok(PolicyDecision::Denied("shared-wallet hard limit exceeded".to_owned()));
        }
        let exceeds_automatic_daily = scenario_after.is_some_and(|total| total > limits.max_per_day);
        if exceeds_automatic_daily && !limits.approval_enabled {
            return Ok(PolicyDecision::Denied("daily scenario budget exceeded".to_owned()));
        }
        if exceeds_automatic_daily || policy.requires_approval(direction, challenge.amount) {
            Ok(PolicyDecision::RequiresApproval)
        } else {
            Ok(PolicyDecision::Allowed)
        }
    }
}

fn exceeds(current: Amount, requested: Amount, limit: Amount) -> bool {
    current.checked_add(requested).is_none_or(|total| total > limit)
}

#[cfg(test)]
#[path = "../../tests/model/policy_test.rs"]
mod engine_tests;
