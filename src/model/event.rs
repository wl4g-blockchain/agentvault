pub use super::domain::{PaymentEvent, PaymentEventType};

/// Explicit delivery result returned by an application/provider sink.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PaymentEventDelivery {
    Delivered,
    Retryable { reason: String },
    PermanentFailure { reason: String },
}

/// Summary of one bounded outbox lease and delivery pass.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PaymentEventDispatchRun {
    pub claimed: u32,
    pub delivered: u32,
    pub rescheduled: u32,
    pub dead_lettered: u32,
}

#[cfg(test)]
#[path = "../../tests/model/event_test.rs"]
mod tests;
