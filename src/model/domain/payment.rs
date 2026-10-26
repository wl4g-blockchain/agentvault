use serde::{Deserialize, Serialize};

use crate::model::{Amount, AssetIdentity, PaymentDirection, ReplayCheck};

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    Created,
    PendingApproval,
    Authorized,
    Signed,
    Submitted,
    PendingSettlement,
    Settled,
    Failed,
    Expired,
    Unknown,
}

impl PaymentStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::PendingApproval => "pending_approval",
            Self::Authorized => "authorized",
            Self::Signed => "signed",
            Self::Submitted => "submitted",
            Self::PendingSettlement => "pending_settlement",
            Self::Settled => "settled",
            Self::Failed => "failed",
            Self::Expired => "expired",
            Self::Unknown => "unknown",
        }
    }

    #[must_use]
    pub const fn is_final(self) -> bool {
        matches!(self, Self::Settled | Self::Failed | Self::Expired)
    }
}

/// Durable state of a budget reservation.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "snake_case")]
pub enum ReservationStatus {
    #[default]
    Unreserved,
    Reserved,
    Consumed,
    Released,
}

impl ReservationStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unreserved => "unreserved",
            Self::Reserved => "reserved",
            Self::Consumed => "consumed",
            Self::Released => "released",
        }
    }
}

crate::storage::repository_entity! { "pay_intent", ReadOnly;
    keys: ["id"], immutable: ["created_at"], revision: None;
    /// Current persisted state of one inbound or outbound payment operation.
    #[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, sqlx::FromRow)]
    #[serde(deny_unknown_fields)]
    pub struct PaymentIntent {
        pub id: String,
        pub scenario_id: String,
        pub run_id: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub wallet_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub policy_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub offer_id: Option<String>,
        pub direction: PaymentDirection,
        pub protocol: String,
        #[sqlx(flatten)]
        pub asset: AssetIdentity,
        pub recipient: String,
        pub amount: Amount,
        pub settled_amount: Amount,
        pub resource: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub challenge_hash: Option<String>,
        pub request_hash: String,
        pub status: PaymentStatus,
        pub reservation_status: ReservationStatus,
        pub reserved_amount: Amount,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub tx_hash: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub settlement_ref: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub next_reconcile_at: Option<i64>,
        pub created_at: i64,
        pub updated_at: i64,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub expires_at: Option<i64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub settled_at: Option<i64>,
    }
}

impl PaymentIntent {
    #[must_use]
    pub fn check_replay(&self, request_hash: &str) -> ReplayCheck {
        if self.request_hash == request_hash {
            ReplayCheck::Replay
        } else {
            ReplayCheck::Conflict
        }
    }

    /// Returns the policy and wallet selected internally by `AgentVault`.
    ///
    /// # Errors
    ///
    /// Returns the missing or empty authority field.
    pub fn authority_ids(&self) -> Result<(&str, &str), &'static str> {
        let policy_id = self
            .policy_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or("policy_id")?;
        let wallet_id = self
            .wallet_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or("wallet_id")?;
        Ok((policy_id, wallet_id))
    }

    #[must_use]
    pub fn same_immutable_identity_as(&self, other: &Self) -> bool {
        self.id == other.id
            && self.scenario_id == other.scenario_id
            && self.run_id == other.run_id
            && self.wallet_id == other.wallet_id
            && self.policy_id == other.policy_id
            && self.offer_id == other.offer_id
            && self.direction == other.direction
            && self.protocol == other.protocol
            && self.asset == other.asset
            && self.recipient == other.recipient
            && self.amount == other.amount
            && self.resource == other.resource
            && self.challenge_hash == other.challenge_hash
            && self.request_hash == other.request_hash
            && self.created_at == other.created_at
            && self.expires_at == other.expires_at
    }

    #[must_use]
    pub const fn needs_reconciliation(&self) -> bool {
        matches!(
            self.status,
            PaymentStatus::Signed
                | PaymentStatus::Submitted
                | PaymentStatus::PendingSettlement
                | PaymentStatus::Unknown
        )
    }

    /// Checks protocol- and storage-independent invariants.
    ///
    /// # Errors
    ///
    /// Returns the invalid field or invariant name.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.id.trim().is_empty() {
            return Err("id");
        }
        if self.scenario_id.trim().is_empty() {
            return Err("scenario_id");
        }
        if self.run_id.trim().is_empty() {
            return Err("run_id");
        }
        self.authority_ids()?;
        match self.direction {
            PaymentDirection::Inbound
                if self
                    .offer_id
                    .as_deref()
                    .is_none_or(|offer_id| offer_id.trim().is_empty()) =>
            {
                return Err("offer_id");
            }
            PaymentDirection::Outbound if self.offer_id.is_some() => return Err("offer_id"),
            _ => {}
        }
        if self.protocol.trim().is_empty() {
            return Err("protocol");
        }
        self.asset.validate()?;
        if self.recipient.trim().is_empty() {
            return Err("recipient");
        }
        if self.amount.is_zero() {
            return Err("amount");
        }
        if self.request_hash.trim().is_empty() {
            return Err("request_hash");
        }
        if self.settled_amount > self.amount {
            return Err("settled_amount");
        }
        if self.reserved_amount > self.amount {
            return Err("reserved_amount");
        }
        if self.status == PaymentStatus::Settled && self.settled_at.is_none() {
            return Err("settled_at");
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../../tests/model/domain/payment_test.rs"]
mod tests;
