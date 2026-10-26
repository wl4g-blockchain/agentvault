//! Price domain service. External integrations belong to dedicated components.
use crate::{
    model::PaymentPrice,
    storage::{IPriceRepository, Page, PageRequest},
    Error, Result,
};
use std::sync::Arc;

pub struct PriceHandler {
    repository: Arc<dyn IPriceRepository>,
}
impl PriceHandler {
    #[must_use]
    pub fn new(repository: Arc<dyn IPriceRepository>) -> Self {
        Self { repository }
    }

    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn register(&self, price: &PaymentPrice) -> Result<PaymentPrice> {
        price
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment price {field}")))?;
        self.repository.insert(price).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn get(&self, id: &str) -> Result<Option<PaymentPrice>> {
        self.repository.get(id).await
    }

    /// # Errors
    /// Rejects ambiguous pricing or returns persistence errors.
    pub async fn resolve_resource(&self, scenario: &str, resource: &str) -> Result<Option<PaymentPrice>> {
        self.repository.resolve_resource(scenario, resource).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn update(&self, price: &PaymentPrice, expected_updated_at: i64) -> Result<PaymentPrice> {
        price
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment price {field}")))?;
        self.repository.update(price, expected_updated_at).await
    }

    /// # Errors
    /// Rejects invalid revisions, concurrent changes, and referenced prices.
    pub async fn delete(&self, id: &str, expected_updated_at: i64) -> Result<()> {
        if id.trim().is_empty() || expected_updated_at < 0 {
            return Err(Error::InvalidRequest("invalid price identity or revision".to_owned()));
        }
        self.repository.delete(id, expected_updated_at).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn list_for_scenario(&self, scenario: &str, page: PageRequest) -> Result<Page<PaymentPrice>> {
        self.repository.list_for_scenario(scenario, page).await
    }
}

#[cfg(test)]
#[path = "../../tests/handler/price_test.rs"]
mod tests;
