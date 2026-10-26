//! Policy domain service. External integrations belong to dedicated components.
use crate::{
    model::PaymentPolicy,
    storage::{IPolicyRepository, Page, PageRequest},
    Error, Result,
};
use std::sync::Arc;

pub struct PolicyHandler {
    repository: Arc<dyn IPolicyRepository>,
}
impl PolicyHandler {
    #[must_use]
    pub fn new(repository: Arc<dyn IPolicyRepository>) -> Self {
        Self { repository }
    }

    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn register(&self, policy: &PaymentPolicy) -> Result<PaymentPolicy> {
        policy
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment policy {field}")))?;
        self.repository.insert(policy).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn get(&self, id: &str) -> Result<Option<PaymentPolicy>> {
        self.repository.get(id).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn update(&self, policy: &PaymentPolicy, expected_updated_at: i64) -> Result<PaymentPolicy> {
        policy
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment policy {field}")))?;
        self.repository.update(policy, expected_updated_at).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn list_for_scenario(&self, scenario: &str, page: PageRequest) -> Result<Page<PaymentPolicy>> {
        self.repository.list_for_scenario(scenario, page).await
    }
    /// # Errors
    /// Returns persistence errors.
    pub async fn has_enabled_wallet_binding(&self, scenario: &str, wallet: &str) -> Result<bool> {
        self.repository.has_enabled_wallet_binding(scenario, wallet).await
    }
}

#[cfg(test)]
#[path = "../../tests/handler/policy_test.rs"]
mod tests;
