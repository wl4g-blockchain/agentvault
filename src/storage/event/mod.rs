//! Event persistence contract.

use crate::{
    model::{PaymentEvent, PaymentEventType},
    storage::{IdempotentWrite, Page, PageRequest},
    Result,
};
use async_trait::async_trait;

pub mod postgres;
pub mod sqlite;

/// Append-only operational history. Events are never used as the authoritative
/// source for wallet totals.
#[async_trait]
pub trait IEventRepository: Send + Sync {
    async fn append(&self, event: &PaymentEvent) -> Result<IdempotentWrite<PaymentEvent>>;

    async fn get(&self, event_id: &str) -> Result<Option<PaymentEvent>>;

    async fn find_for_intent_type(
        &self,
        intent_id: &str,
        event_type: PaymentEventType,
        attempt_no: u32,
    ) -> Result<Option<PaymentEvent>>;

    async fn list_for_intent(&self, intent_id: &str, page: PageRequest) -> Result<Page<PaymentEvent>>;
}

#[cfg(test)]
#[path = "../../../tests/storage/event/mod_test.rs"]
mod tests;

/// Compatibility import for the single repository contract.
pub use IEventRepository as EventStorage;
