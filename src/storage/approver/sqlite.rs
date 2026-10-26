//! Scenario approver repository; identity and revision are checked atomically.
use crate::{
    model::PaymentScenarioApprover,
    storage::{
        sqlite::{dynamic_sqlite, BaseSqliteRepository},
        ApproverStorage, EntityComparison, EntityQuery, Page, PageRequest,
    },
    Error, Result,
};
use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;
#[derive(Clone)]
pub struct ApproverSqliteRepository {
    base: BaseSqliteRepository,
}
#[async_trait]
impl ApproverStorage for ApproverSqliteRepository {
    async fn upsert(&self, approver: &PaymentScenarioApprover) -> Result<PaymentScenarioApprover> {
        approver
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid scenario approver {field}")))?;
        let row = dynamic_sqlite!(
            upsert,
            approver,
            &["scenario_id", "auth_principal"],
            &["enabled", "updated_at"],
            &["created_at"],
            "updated_at"
        )?
        .push(" RETURNING *")
        .build()
        .fetch_optional(self.base.pool())
        .await
        .map_err(|error| crate::Error::conflict(format!("scenario approver upsert rejected: {error}")))?
        .ok_or_else(|| crate::Error::conflict("scenario approver identity or revision changed"))?;
        Self::decode_row(&row)
    }
    async fn get(&self, scenario_id: &str, principal: &str) -> Result<Option<PaymentScenarioApprover>> {
        self.base
            .select_one(
                EntityQuery::new()
                    .eq("scenario_id", scenario_id)
                    .eq("auth_principal", principal),
            )
            .await
    }
    async fn is_authorized(&self, scenario_id: &str, auth_principal: &str) -> Result<bool> {
        let authorized: i64 = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM pay_scenario_approver a \
             JOIN pay_scenario s ON s.id = a.scenario_id JOIN pay_client c ON c.id = s.client_id \
             WHERE a.scenario_id = ?1 AND a.auth_principal = ?2 \
             AND a.enabled = 1 AND s.enabled = 1 AND c.enabled = 1)",
        )
        .bind(scenario_id)
        .bind(auth_principal)
        .fetch_one(self.base.pool())
        .await
        .map_err(|error| BaseSqliteRepository::storage_error("authorize scenario approver", error))?;
        Ok(authorized == 1)
    }
    async fn set_enabled(
        &self,
        scenario_id: &str,
        principal: &str,
        enabled: bool,
        updated_at: i64,
    ) -> Result<PaymentScenarioApprover> {
        self.base
            .entity_update_returning::<PaymentScenarioApprover>(
                crate::storage::EntityValues::from([("enabled", enabled.into()), ("updated_at", updated_at.into())]),
                EntityQuery::new()
                    .eq("scenario_id", scenario_id)
                    .eq("auth_principal", principal)
                    .compare("updated_at", EntityComparison::Less, updated_at),
            )
            .await?
            .ok_or_else(|| crate::Error::conflict("scenario approver was not found or revision did not advance"))
    }
    async fn list_for_scenario(&self, scenario_id: &str, page: PageRequest) -> Result<Page<PaymentScenarioApprover>> {
        self.base
            .entity_page(
                EntityQuery::new()
                    .eq("scenario_id", scenario_id)
                    .order("created_at", false)
                    .order("auth_principal", false),
                page,
            )
            .await
    }
}
impl ApproverSqliteRepository {
    pub(crate) fn new(base: BaseSqliteRepository) -> Self {
        Self { base }
    }
    pub(crate) fn decode_row(row: &SqliteRow) -> Result<PaymentScenarioApprover> {
        sqlx::FromRow::from_row(row)
            .map_err(|error| BaseSqliteRepository::storage_error("decode scenario approver", error))
    }
}
#[cfg(test)]
#[path = "../../../tests/storage/approver/sqlite_test.rs"]
mod tests;
