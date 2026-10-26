use serde::{Deserialize, Serialize};

use crate::{
    model::{Amount, ApprovalDecision, ApprovalDecisionKind},
    utils::secure_material::SealedSensitiveMaterial,
};

crate::storage::repository_entity! { "pay_scenario_approver", Catalog;
    keys: ["scenario_id", "auth_principal"], immutable: ["created_at"], revision: Some("updated_at");
    /// Trusted administrator mapping persisted for one payment scenario.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, sqlx::FromRow)]
    #[serde(deny_unknown_fields)]
    pub struct PaymentScenarioApprover {
        pub scenario_id: String,
        pub auth_principal: String,
        pub enabled: bool,
        pub created_at: i64,
        pub updated_at: i64,
    }
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

crate::storage::repository_entity! { "pay_approval", ReadOnly;
    keys: ["id"], immutable: ["created_at"], revision: None;
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

    /// Verifies that a decision is bound to this row and does not increase the
    /// requested amount.
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

#[cfg(test)]
#[path = "../../../tests/model/domain/approval_test.rs"]
mod tests;
