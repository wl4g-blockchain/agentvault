//! Price persistence for `sqlite`.

use crate::{
    model::PaymentPrice,
    storage::{policy::sqlite::PolicySqliteRepository, sqlite::SqliteRepository, IPriceRepository, Page, PageRequest},
    Error, Result,
};
use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

#[derive(Clone)]
pub struct PriceSqliteRepository {
    base: SqliteRepository,
}

#[async_trait]
impl IPriceRepository for PriceSqliteRepository {
    async fn insert(&self, price: &PaymentPrice) -> Result<PaymentPrice> {
        Self::validate_price(price)?;
        let price = price.clone();
        self.base
            .immediate(|connection| {
                Box::pin(async move {
                    Self::validate_price_authority(connection, &price).await?;
                    Self::insert_price(connection, &price).await?;
                    Ok(price)
                })
            })
            .await
    }

    async fn get(&self, price_id: &str) -> Result<Option<PaymentPrice>> {
        self.base
            .select_one(crate::storage::EntityQuery::new().eq("id", price_id))
            .await
    }

    async fn resolve(&self, scenario_id: &str, resource: &str, protocol: &str) -> Result<Option<PaymentPrice>> {
        let rows = sqlx::query(
            "SELECT * FROM pay_price WHERE scenario_id = ?1 AND resource = ?2 AND enabled = 1 \
             AND EXISTS (SELECT 1 FROM json_each(protocols) WHERE value = ?3)",
        )
        .bind(scenario_id)
        .bind(resource)
        .bind(protocol)
        .fetch_all(self.base.pool())
        .await
        .map_err(|error| SqliteRepository::storage_error("resolve payment price", error))?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => Self::decode_row(row).map(Some),
            _ => Err(Error::Storage(
                "multiple enabled prices own one scenario resource".to_owned(),
            )),
        }
    }

    async fn resolve_resource(&self, scenario_id: &str, resource: &str) -> Result<Option<PaymentPrice>> {
        self.base
            .entity_unique(
                crate::storage::EntityQuery::new()
                    .eq("scenario_id", scenario_id)
                    .eq("resource", resource)
                    .eq("enabled", true),
            )
            .await
    }

    async fn update(&self, price: &PaymentPrice, expected_updated_at: i64) -> Result<PaymentPrice> {
        Self::validate_price(price)?;
        if price.updated_at <= expected_updated_at {
            return Err(crate::Error::conflict("payment price update timestamp did not advance"));
        }
        let price = price.clone();
        self.base
            .immediate(|connection| {
                Box::pin(async move {
                    Self::validate_price_authority(connection, &price).await?;
                    let mut changes = crate::storage::RepositoryEntity::values(&price)?;
                    for field in ["id", "scenario_id", "policy_id", "created_at"] {
                        changes.remove(field);
                    }
                    let filters = crate::storage::EntityQuery::new()
                        .eq("id", &price.id)
                        .eq("scenario_id", &price.scenario_id)
                        .eq("policy_id", &price.policy_id)
                        .eq("created_at", price.created_at)
                        .eq("updated_at", expected_updated_at);
                    let result = crate::storage::sqlite::dynamic_sqlite!(update, PaymentPrice, changes, filters)?
                        .build()
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| crate::Error::conflict(format!("payment price update rejected: {error}")))?;
                    if result.rows_affected() != 1 {
                        return Err(crate::Error::conflict("payment price identity or revision changed"));
                    }
                    Ok(price)
                })
            })
            .await
    }

    async fn delete(&self, price_id: &str, expected_updated_at: i64) -> Result<()> {
        let filters = crate::storage::EntityQuery::new()
            .eq("id", price_id)
            .eq("updated_at", expected_updated_at);
        let result = crate::storage::sqlite::dynamic_sqlite!(delete_by, PaymentPrice, filters)?
            .build()
            .execute(self.base.pool())
            .await
            .map_err(|error| Error::conflict(format!("price deletion rejected: {error}")))?;
        if result.rows_affected() != 1 {
            return Err(Error::conflict("price revision changed or price does not exist"));
        }
        Ok(())
    }

    async fn list_for_scenario(&self, scenario_id: &str, page: PageRequest) -> Result<Page<PaymentPrice>> {
        self.base
            .entity_page(
                crate::storage::EntityQuery::new()
                    .eq("scenario_id", scenario_id)
                    .order("created_at", false)
                    .order("id", false),
                page,
            )
            .await
    }
}

impl PriceSqliteRepository {
    pub(crate) fn new(base: SqliteRepository) -> Self {
        Self { base }
    }

    fn validate_price(price: &PaymentPrice) -> Result<()> {
        price
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment price {field}")))
    }

    async fn validate_price_authority(connection: &mut sqlx::SqliteConnection, price: &PaymentPrice) -> Result<()> {
        let wallet: Option<String> = sqlx::query_scalar("SELECT id FROM pay_wallet WHERE id = ?1")
            .bind(&price.wallet_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| SqliteRepository::storage_error("load price payment wallet", error))?;
        if wallet.is_none() {
            return Err(Error::NotFound(format!("payment wallet {}", price.wallet_id)));
        }
        let policy = PolicySqliteRepository::load_policy_connection(connection, &price.policy_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("payment policy {}", price.policy_id)))?;
        let inbound = policy
            .inbound_limits
            .as_ref()
            .ok_or_else(|| crate::Error::conflict("price policy does not authorize inbound payments"))?;
        if policy.scenario_id != price.scenario_id
            || !policy.wallet_ids.contains(&price.wallet_id)
            || !policy.budget_unit.matches(&price.asset)
            || !policy.networks.contains(&price.asset.network)
            || !policy.assets.contains(&price.asset)
            || price.amount > inbound.max_per_payment
            || price
                .protocols
                .iter()
                .any(|protocol| !policy.protocols.contains(protocol))
        {
            return Err(crate::Error::conflict(
                "price exceeds or falls outside its inbound policy scope",
            ));
        }
        Ok(())
    }

    async fn insert_price(connection: &mut sqlx::SqliteConnection, price: &PaymentPrice) -> Result<()> {
        crate::storage::sqlite::dynamic_sqlite!(insert, price)?
            .build()
            .execute(connection)
            .await
            .map_err(|error| crate::Error::conflict(format!("payment price insert rejected: {error}")))?;
        Ok(())
    }

    pub(crate) fn decode_row(row: &SqliteRow) -> Result<PaymentPrice> {
        sqlx::FromRow::from_row(row).map_err(|error| SqliteRepository::storage_error("decode price entity", error))
    }
}

#[cfg(test)]
#[path = "../../../tests/storage/price/sqlite_test.rs"]
mod tests;
