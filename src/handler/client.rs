//! Client domain service. External integrations belong to dedicated components.
use crate::{
    model::PaymentClient,
    storage::{IClientRepository, Page, PageRequest},
    Error, Result,
};
use std::sync::Arc;

pub struct PayClientHandler {
    repository: Arc<dyn IClientRepository>,
}
impl PayClientHandler {
    #[must_use]
    pub fn new(repository: Arc<dyn IClientRepository>) -> Self {
        Self { repository }
    }

    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn authenticate(&self, principal: &str) -> Result<PaymentClient> {
        if principal.trim().is_empty() {
            return Err(Error::Unauthenticated);
        }
        self.repository
            .find_by_principal(principal)
            .await?
            .filter(|client| client.authenticates(principal))
            .ok_or(Error::Forbidden)
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn register(&self, client: &PaymentClient) -> Result<PaymentClient> {
        client
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment client {field}")))?;
        self.repository.insert(client).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn get(&self, id: &str) -> Result<Option<PaymentClient>> {
        self.repository.get(id).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn list(&self, page: PageRequest) -> Result<Page<PaymentClient>> {
        self.repository.list(page).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn set_enabled(&self, id: &str, enabled: bool, now: i64) -> Result<PaymentClient> {
        self.repository.set_enabled(id, enabled, now).await
    }
}

#[cfg(test)]
#[path = "../../tests/handler/client_test.rs"]
mod tests;
