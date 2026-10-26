//! Client persistence contract.

use crate::{
    model::PaymentClient,
    storage::{Page, PageRequest},
    Result,
};
use async_trait::async_trait;

pub mod postgres;
pub mod sqlite;

#[async_trait]
pub trait IClientRepository: Send + Sync {
    async fn insert(&self, client: &PaymentClient) -> Result<PaymentClient>;

    async fn get(&self, client_id: &str) -> Result<Option<PaymentClient>>;

    async fn find_by_principal(&self, auth_principal: &str) -> Result<Option<PaymentClient>>;

    async fn set_enabled(&self, client_id: &str, enabled: bool, updated_at: i64) -> Result<PaymentClient>;

    async fn list(&self, page: PageRequest) -> Result<Page<PaymentClient>>;
}

#[cfg(test)]
#[path = "../../../tests/storage/client/mod_test.rs"]
mod tests;

/// Compatibility import for the single repository contract.
pub use IClientRepository as ClientStorage;
