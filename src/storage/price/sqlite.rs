//! Price persistence for `sqlite`.

use crate::{
    model::PaymentPrice,
    storage::{policy::sqlite::PolicySqliteRepository, sqlite::BaseSqliteRepository, Page, PageRequest, PriceStorage},
    Error, Result,
};
use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

#[derive(Clone)]
pub struct PriceSqliteRepository {
    base: BaseSqliteRepository,
}

#[async_trait]
impl PriceStorage for PriceSqliteRepository {
    async fn insert(&self, offer: &PaymentPrice) -> Result<PaymentPrice> {
        Self::validate_offer(offer)?;
        let offer = offer.clone();
        self.base
            .immediate(|connection| {
                Box::pin(async move {
                    Self::validate_offer_authority(connection, &offer).await?;
                    Self::insert_offer(connection, &offer).await?;
                    Ok(offer)
                })
            })
            .await
    }

    async fn get(&self, offer_id: &str) -> Result<Option<PaymentPrice>> {
        self.base
            .select_one(crate::storage::EntityQuery::new().eq("id", offer_id))
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
        .map_err(|error| BaseSqliteRepository::storage_error("resolve payment offer", error))?;
        match rows.as_slice() {
            [] => Ok(None),
            [row] => Self::decode_row(row).map(Some),
            _ => Err(Error::Storage(
                "multiple enabled offers own one scenario resource".to_owned(),
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

    async fn update(&self, offer: &PaymentPrice, expected_updated_at: i64) -> Result<PaymentPrice> {
        Self::validate_offer(offer)?;
        if offer.updated_at <= expected_updated_at {
            return Err(crate::Error::conflict("payment offer update timestamp did not advance"));
        }
        let offer = offer.clone();
        self.base
            .immediate(|connection| {
                Box::pin(async move {
                    Self::validate_offer_authority(connection, &offer).await?;
                    let mut changes = crate::storage::RepositoryEntity::values(&offer)?;
                    for field in ["id", "scenario_id", "policy_id", "created_at"] {
                        changes.remove(field);
                    }
                    let filters = crate::storage::EntityQuery::new()
                        .eq("id", &offer.id)
                        .eq("scenario_id", &offer.scenario_id)
                        .eq("policy_id", &offer.policy_id)
                        .eq("created_at", offer.created_at)
                        .eq("updated_at", expected_updated_at);
                    let result = crate::storage::sqlite::dynamic_sqlite!(update, PaymentPrice, changes, filters)?
                        .build()
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| crate::Error::conflict(format!("payment offer update rejected: {error}")))?;
                    if result.rows_affected() != 1 {
                        return Err(crate::Error::conflict("payment offer identity or revision changed"));
                    }
                    Ok(offer)
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
    pub(crate) fn new(base: BaseSqliteRepository) -> Self {
        Self { base }
    }

    fn validate_offer(offer: &PaymentPrice) -> Result<()> {
        offer
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment offer {field}")))
    }

    async fn validate_offer_authority(connection: &mut sqlx::SqliteConnection, offer: &PaymentPrice) -> Result<()> {
        let wallet: Option<String> = sqlx::query_scalar("SELECT id FROM pay_wallet WHERE id = ?1")
            .bind(&offer.wallet_id)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| BaseSqliteRepository::storage_error("load offer payment wallet", error))?;
        if wallet.is_none() {
            return Err(Error::NotFound(format!("payment wallet {}", offer.wallet_id)));
        }
        let policy = PolicySqliteRepository::load_policy_connection(connection, &offer.policy_id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("payment policy {}", offer.policy_id)))?;
        let inbound = policy
            .inbound_limits
            .as_ref()
            .ok_or_else(|| crate::Error::conflict("offer policy does not authorize inbound payments"))?;
        if policy.scenario_id != offer.scenario_id
            || !policy.wallet_ids.contains(&offer.wallet_id)
            || !policy.budget_unit.matches(&offer.asset)
            || !policy.networks.contains(&offer.asset.network)
            || !policy.assets.contains(&offer.asset)
            || offer.amount > inbound.max_per_payment
            || offer
                .protocols
                .iter()
                .any(|protocol| !policy.protocols.contains(protocol))
        {
            return Err(crate::Error::conflict(
                "offer exceeds or falls outside its inbound policy scope",
            ));
        }
        Ok(())
    }

    async fn insert_offer(connection: &mut sqlx::SqliteConnection, offer: &PaymentPrice) -> Result<()> {
        crate::storage::sqlite::dynamic_sqlite!(insert, offer)?
            .build()
            .execute(connection)
            .await
            .map_err(|error| crate::Error::conflict(format!("payment offer insert rejected: {error}")))?;
        Ok(())
    }

    pub(crate) fn decode_row(row: &SqliteRow) -> Result<PaymentPrice> {
        sqlx::FromRow::from_row(row).map_err(|error| BaseSqliteRepository::storage_error("decode offer entity", error))
    }
}

#[cfg(test)]
#[path = "../../../tests/storage/price/sqlite_test.rs"]
mod tests;
