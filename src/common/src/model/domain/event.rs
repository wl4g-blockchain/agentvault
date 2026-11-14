use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PaymentEventType {
    ChallengeCreated,
    ChallengeReceived,
    SignRequested,
    Signed,
    ApprovalRequested,
    ApprovalGranted,
    ApprovalRejected,
    ApprovalConsumed,
    CredentialVerified,
    Submitted,
    SettlementRequested,
    Settled,
    Retried,
    Failed,
    Reconciled,
}

impl PaymentEventType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ChallengeCreated => "challenge_created",
            Self::ChallengeReceived => "challenge_received",
            Self::SignRequested => "sign_requested",
            Self::Signed => "signed",
            Self::ApprovalRequested => "approval_requested",
            Self::ApprovalGranted => "approval_granted",
            Self::ApprovalRejected => "approval_rejected",
            Self::ApprovalConsumed => "approval_consumed",
            Self::CredentialVerified => "credential_verified",
            Self::Submitted => "submitted",
            Self::SettlementRequested => "settlement_requested",
            Self::Settled => "settled",
            Self::Retried => "retried",
            Self::Failed => "failed",
            Self::Reconciled => "reconciled",
        }
    }
}

crate::storage::repository_entity! { "pay_event", AppendOnly;
    keys: ["id"], immutable: ["created_at"], revision: None;
    /// Immutable operational event. This is audit history, not the source of truth
    /// for confirmed financial totals.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, sqlx::FromRow)]
    #[serde(deny_unknown_fields)]
    pub struct PaymentEvent {
        pub id: String,
        pub intent_id: String,
        pub event_type: PaymentEventType,
        #[sqlx(try_from = "i64")]
        pub attempt_no: u32,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub signature_ref: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tx_hash: Option<String>,
        #[sqlx(json)]
        pub payload: Value,
        pub created_at: i64,
    }
}

impl PaymentEvent {
    /// Checks identifiers required for append-only persistence.
    ///
    /// # Errors
    ///
    /// Returns the invalid field name.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.trim().is_empty() {
            return Err("id");
        }
        if self.intent_id.trim().is_empty() {
            return Err("intent_id");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../tests/model/domain/event_test.rs"]
mod tests;
