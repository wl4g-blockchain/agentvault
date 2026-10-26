//! Event domain service. External integrations belong to dedicated components.
use crate::{
    model::{PaymentEvent, PaymentEventType},
    storage::{IEventRepository, IdempotentWrite, Page, PageRequest},
    Result,
};
use std::sync::Arc;

pub struct EventHandler {
    repository: Arc<dyn IEventRepository>,
}
impl EventHandler {
    #[must_use]
    pub fn new(repository: Arc<dyn IEventRepository>) -> Self {
        Self { repository }
    }

    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn append(&self, event: &PaymentEvent) -> Result<IdempotentWrite<PaymentEvent>> {
        self.repository.append(event).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn get(&self, id: &str) -> Result<Option<PaymentEvent>> {
        self.repository.get(id).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn list_for_intent(&self, intent: &str, page: PageRequest) -> Result<Page<PaymentEvent>> {
        self.repository.list_for_intent(intent, page).await
    }
    /// # Errors
    /// Returns persistence errors.
    pub async fn find_for_intent_type(
        &self,
        intent: &str,
        event_type: PaymentEventType,
        attempt: u32,
    ) -> Result<Option<PaymentEvent>> {
        self.repository.find_for_intent_type(intent, event_type, attempt).await
    }
}

#[cfg(test)]
#[path = "../../tests/handler/event_test.rs"]
mod tests;
