//! Intent domain service. External integrations belong to dedicated components.
use crate::{
    model::PaymentIntent,
    storage::{IIntentRepository, Page, PaymentQuery},
    Result,
};
use std::sync::Arc;

pub struct IntentHandler {
    repository: Arc<dyn IIntentRepository>,
}
impl IntentHandler {
    #[must_use]
    pub fn new(repository: Arc<dyn IIntentRepository>) -> Self {
        Self { repository }
    }

    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn get(&self, id: &str) -> Result<Option<PaymentIntent>> {
        self.repository.get(id).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn get_for_scenario(&self, scenario: &str, id: &str) -> Result<Option<PaymentIntent>> {
        self.repository.get_for_scenario(scenario, id).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn list(&self, query: &PaymentQuery) -> Result<Page<PaymentIntent>> {
        self.repository.list(query).await
    }
}

#[cfg(test)]
#[path = "../../tests/handler/intent_test.rs"]
mod tests;
