//! Durable rows in `pay_outbox`, committed atomically with their aggregate.
use serde_json::Value;
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
