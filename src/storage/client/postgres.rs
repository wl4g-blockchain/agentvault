//! Client repository; ordinary CRUD is generated from `PaymentClient`.
use crate::{
    model::PaymentClient,
    storage::{postgres::BasePostgresRepository, ClientStorage, EntityComparison, EntityQuery, Page, PageRequest},
    Error, Result,
};
use async_trait::async_trait;
#[derive(Clone)]
pub struct ClientPostgresRepository {
    base: BasePostgresRepository,
}
#[async_trait]
impl ClientStorage for ClientPostgresRepository {
    async fn insert(&self, client: &PaymentClient) -> Result<PaymentClient> {
        client
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment client {field}")))?;
        self.base
            .entity_insert_returning(client)
            .await
            .map_err(crate::utils::storage::PaymentStorageCodec::domain_error)
    }
    async fn get(&self, client_id: &str) -> Result<Option<PaymentClient>> {
        self.base.select_one(EntityQuery::new().eq("id", client_id)).await
    }
    async fn find_by_principal(&self, principal: &str) -> Result<Option<PaymentClient>> {
        self.base
            .select_one(EntityQuery::new().eq("auth_principal", principal))
            .await
    }
    async fn set_enabled(&self, client_id: &str, enabled: bool, updated_at: i64) -> Result<PaymentClient> {
        self.base
            .entity_update_returning::<PaymentClient>(
                crate::storage::EntityValues::from([("enabled", enabled.into()), ("updated_at", updated_at.into())]),
                EntityQuery::new()
                    .eq("id", client_id)
                    .compare("updated_at", EntityComparison::LessOrEqual, updated_at),
            )
            .await?
            .ok_or_else(|| crate::Error::conflict("payment client was not found or update timestamp moved backwards"))
    }
    async fn list(&self, page: PageRequest) -> Result<Page<PaymentClient>> {
        self.base
            .entity_page(EntityQuery::new().order("created_at", false).order("id", false), page)
            .await
    }
}
impl ClientPostgresRepository {
    pub(crate) fn new(base: BasePostgresRepository) -> Self {
        Self { base }
    }
}
#[cfg(test)]
#[path = "../../../tests/storage/client/postgres_test.rs"]
mod tests;
