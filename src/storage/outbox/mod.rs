//! Outbox persistence contract.

use crate::Result;
use async_trait::async_trait;
use serde_json::Value;

pub mod postgres;
pub mod sqlite;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxStatus {
    Pending,
    Delivered,
    DeadLetter,
}

impl OutboxStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Delivered => "delivered",
            Self::DeadLetter => "dead_letter",
        }
    }
}

/// Closed delivery families used to partition one durable outbox between
/// independent workers. Callers cannot supply arbitrary SQL fragments or
/// destination prefixes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxDeliveryClass {
    /// Administrator-facing MQTT notifications for one client.
    ApprovalRequired,
    /// Durable payment lifecycle events for application/provider dispatchers.
    PaymentEvents,
}

impl OutboxDeliveryClass {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ApprovalRequired => "approval_required",
            Self::PaymentEvents => "payment_events",
        }
    }

    /// Mirrors the controlled predicates implemented by SQL adapters.
    #[must_use]
    pub fn matches(self, message: &OutboxMessage) -> bool {
        match self {
            Self::ApprovalRequired => {
                message.event_type == "approval_required"
                    && Self::destination_has_nonempty_id(&message.destination, "client:", ".approvals")
            }
            Self::PaymentEvents => {
                Self::destination_has_nonempty_id(&message.destination, "client:", ".payments")
                    || Self::destination_has_nonempty_id(&message.destination, "scenario:", ".payments")
            }
        }
    }
    fn destination_has_nonempty_id(destination: &str, prefix: &str, suffix: &str) -> bool {
        destination
            .strip_prefix(prefix)
            .and_then(|value| value.strip_suffix(suffix))
            .is_some_and(|identifier| !identifier.is_empty())
    }
}

/// Durable message inserted in the same transaction as its aggregate change.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxMessage {
    pub id: String,
    pub aggregate_type: String,
    pub aggregate_id: String,
    pub event_type: String,
    pub destination: String,
    /// Business run that produced this event. Together with
    /// `destination` and `event_type` this is the durable delivery
    /// identity; retries increment `attempt_count` without inventing
    /// another business key.
    pub run_id: String,
    pub payload: Value,
    pub status: OutboxStatus,
    pub attempt_count: u32,
    pub available_at: i64,
    pub lease_owner: Option<String>,
    pub lease_expires_at: Option<i64>,
    pub last_error: Option<String>,
    pub created_at: i64,
    pub delivered_at: Option<i64>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OutboxLeaseRequest {
    pub owner: String,
    pub class: OutboxDeliveryClass,
    pub now: i64,
    pub lease_until: i64,
    pub limit: u32,
}

/// Persistent delivery queue. `PostgreSQL` implementations should claim with
/// `FOR UPDATE SKIP LOCKED`; `SQLite` implementations use a short write
/// transaction and conditional lease update.
#[async_trait]
pub trait IOutboxRepository: Send + Sync {
    /// Claims only rows in `request.class`. A worker must never lease, mutate,
    /// or consume another delivery family's rows.
    async fn claim(&self, request: &OutboxLeaseRequest) -> Result<Vec<OutboxMessage>>;

    async fn mark_delivered(&self, message_id: &str, owner: &str, delivered_at: i64) -> Result<()>;

    async fn reschedule(&self, message_id: &str, owner: &str, available_at: i64, error: &str) -> Result<()>;

    async fn mark_dead_letter(&self, message_id: &str, owner: &str, error: &str) -> Result<()>;
}

#[cfg(test)]
#[path = "../../../tests/storage/outbox/mod_test.rs"]
mod tests;

mod validation {
    use crate::{storage::OutboxMessage, Error, Result};

    pub(crate) struct OutboxStorageRules;
    impl OutboxStorageRules {
        pub(crate) fn same_logical_message(left: &OutboxMessage, right: &OutboxMessage) -> bool {
            left.id == right.id
                && left.aggregate_type == right.aggregate_type
                && left.aggregate_id == right.aggregate_id
                && left.event_type == right.event_type
                && left.destination == right.destination
                && left.run_id == right.run_id
                && left.payload == right.payload
                && left.created_at == right.created_at
        }
        pub(crate) fn validate_message(message: &OutboxMessage) -> Result<()> {
            if message.id.trim().is_empty()
                || message.aggregate_type.trim().is_empty()
                || message.aggregate_id.trim().is_empty()
                || message.event_type.trim().is_empty()
                || message.destination.trim().is_empty()
                || message.run_id.trim().is_empty()
            {
                return Err(Error::InvalidRequest(
                    "outbox message identity is incomplete".to_owned(),
                ));
            }
            Ok(())
        }
    }
}
pub(crate) use validation::*;
