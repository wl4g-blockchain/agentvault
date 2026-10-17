use std::fmt;

use serde::{Deserialize, Serialize};

use super::payment::Amount;
use crate::utils::secure_material::SealedSensitiveMaterial;

/// Trusted administrator mapping for one payment scenario. The principal is
/// resolved by authentication middleware and is never accepted from a request
/// body.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PaymentScenarioApprover {
    pub scenario_id: String,
    pub auth_principal: String,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl PaymentScenarioApprover {
    /// Validates the durable authorization identity.
    ///
    /// # Errors
    ///
    /// Returns the invalid field name.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.scenario_id.trim().is_empty() {
            return Err("scenario_id");
        }
        if self.auth_principal.trim().is_empty() {
            return Err("auth_principal");
        }
        if self.updated_at < self.created_at {
            return Err("updated_at");
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalStatus {
    Pending,
    Approved,
    Rejected,
    Expired,
    Consumed,
}

impl ApprovalStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Expired => "expired",
            Self::Consumed => "consumed",
        }
    }
}

/// Durable approval row for one payment intent.
///
/// The complete JWT is retained only as an authenticated encrypted envelope.
/// Serde deliberately excludes that envelope so API responses, events, and
/// logs cannot expose even the persisted ciphertext by serializing this model.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ApprovalRequest {
    pub id: String,
    pub intent_id: String,
    /// Canonical business run shared with the bound payment intent.
    pub run_id: String,
    pub requested_amount: Amount,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_amount: Option<Amount>,
    pub requested_by: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_by: Option<String>,
    pub status: ApprovalStatus,
    pub intent_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_jti: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub grant_claims_hash: Option<String>,
    #[serde(skip)]
    pub sealed_grant_token: Option<SealedSensitiveMaterial>,
    pub expires_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub consumed_at: Option<i64>,
    pub created_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approved_at: Option<i64>,
}

impl ApprovalRequest {
    #[must_use]
    pub fn is_pending_at(&self, now: i64) -> bool {
        self.status == ApprovalStatus::Pending && self.expires_at > now
    }

    #[must_use]
    pub fn has_persisted_grant(&self) -> bool {
        self.grant_jti.is_some() && self.grant_claims_hash.is_some() && self.sealed_grant_token.is_some()
    }

    /// Verifies that an approval decision is bound to this request and does not
    /// increase the requested amount.
    ///
    /// # Errors
    ///
    /// Returns the failed binding or lifecycle invariant.
    pub fn validate_decision(&self, decision: &ApprovalDecision, now: i64) -> Result<(), &'static str> {
        if self.id != decision.approval_id {
            return Err("approval_id");
        }
        if self.intent_id != decision.intent_id {
            return Err("intent_id");
        }
        if !self.is_pending_at(now) {
            return Err("approval_not_pending");
        }
        match decision.decision {
            ApprovalDecisionKind::Approve => {
                let amount = decision.approved_amount.ok_or("approved_amount")?;
                if amount.is_zero() || amount > self.requested_amount {
                    return Err("approved_amount");
                }
            }
            ApprovalDecisionKind::Reject => {
                if decision.approved_amount.is_some() {
                    return Err("approved_amount");
                }
            }
        }
        Ok(())
    }
}

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
