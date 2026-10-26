use std::fmt;

use serde::{Deserialize, Serialize};

use super::payment::Amount;
use crate::utils::secure_material::SealedSensitiveMaterial;

pub use super::domain::{ApprovalRequest, ApprovalStatus, PaymentScenarioApprover};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecisionKind {
    Approve,
    Reject,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApprovalDecision {
    pub approval_id: String,
    pub intent_id: String,
    pub decision: ApprovalDecisionKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_amount: Option<Amount>,
    pub approved_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub decided_at: i64,
}

/// Authenticated command submitted by an administrator.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SubmitApprovalDecision {
    pub approval_id: String,
    pub intent_id: String,
    pub decision: ApprovalDecisionKind,
    pub approved_amount: Option<Amount>,
    pub reason: Option<String>,
}

/// Approval command result. The durable entity is reused rather than copied
/// into a parallel response shape.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApprovalDecisionResult {
    pub approval: ApprovalRequest,
}

/// Summary for one bounded approval-expiration pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApprovalExpirationRun {
    pub expired: u32,
}

/// Signed JWT claims for an internal, one-time approval grant.
///
/// Registered claims use their standard JWT names. Private claims bind the
/// token to exactly one approval, immutable payment intent, amount, and
/// authenticated approver.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApprovalGrantClaims {
    pub iss: String,
    pub aud: String,
    pub jti: String,
    pub approval_id: String,
    pub intent_id: String,
    pub intent_hash: String,
    pub amount: Amount,
    pub approver: String,
    pub iat: i64,
    pub exp: i64,
}

impl ApprovalGrantClaims {
    /// Validates protocol-independent claim shape and lifetime ordering.
    ///
    /// # Errors
    ///
    /// Returns the invalid claim name.
    pub fn validate(&self) -> Result<(), &'static str> {
        for (field, value) in [
            ("iss", self.iss.as_str()),
            ("aud", self.aud.as_str()),
            ("jti", self.jti.as_str()),
            ("approval_id", self.approval_id.as_str()),
            ("intent_id", self.intent_id.as_str()),
            ("intent_hash", self.intent_hash.as_str()),
            ("approver", self.approver.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(field);
            }
        }
        if self.amount.is_zero() {
            return Err("amount");
        }
        if self.iat < 0 {
            return Err("iat");
        }
        if self.exp <= self.iat {
            return Err("exp");
        }
        Ok(())
    }
}

/// Newly issued grant material passed only to the atomic approval write.
/// The plaintext JWT is never part of this value.
#[derive(Clone, PartialEq, Eq)]
pub struct ApprovalGrant {
    pub claims: ApprovalGrantClaims,
    pub claims_hash: String,
    pub sealed_token: SealedSensitiveMaterial,
}

impl fmt::Debug for ApprovalGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ApprovalGrant")
            .field("jti", &self.claims.jti)
            .field("approval_id", &self.claims.approval_id)
            .field("intent_id", &self.claims.intent_id)
            .field("claims_hash", &self.claims_hash)
            .field("sealed_token", &self.sealed_token)
            .finish_non_exhaustive()
    }
}

/// Authenticated claims recovered from the sealed JWT immediately before the
/// authoritative single-use CAS and budget reservation transaction.
#[derive(Clone, PartialEq, Eq)]
pub struct VerifiedApprovalGrant {
    pub claims: ApprovalGrantClaims,
    pub claims_hash: String,
}

impl fmt::Debug for VerifiedApprovalGrant {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VerifiedApprovalGrant")
            .field("jti", &self.claims.jti)
            .field("approval_id", &self.claims.approval_id)
            .field("intent_id", &self.claims.intent_id)
            .field("claims_hash", &self.claims_hash)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "../../tests/model/approval_test.rs"]
mod tests;
