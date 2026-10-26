//! Scenario persistence for `sqlite`.

use crate::{
    model::{PaymentPolicy, PaymentScenario},
    storage::{
        policy::sqlite::PolicySqliteRepository, sqlite::BaseSqliteRepository, IdempotentWrite, Page, PageRequest,
        ScenarioConfiguration, ScenarioStorage,
    },
    Error, Result,
};
use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

#[derive(Clone)]
pub struct ScenarioSqliteRepository {
    base: BaseSqliteRepository,
}

#[async_trait]
impl ScenarioStorage for ScenarioSqliteRepository {
    async fn register(&self, scenario: &PaymentScenario) -> Result<IdempotentWrite<PaymentScenario>> {
        scenario
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment scenario {field}")))?;
        self.base
            .entity_register(scenario)
            .await
            .map_err(crate::utils::storage::PaymentStorageCodec::domain_error)
    }

    async fn register_with_policy(
        &self,
        scenario: &PaymentScenario,
        policy: &PaymentPolicy,
    ) -> Result<IdempotentWrite<ScenarioConfiguration>> {
        scenario
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment scenario {field}")))?;
        PolicySqliteRepository::validate_policy(policy)?;
        if scenario.id != policy.scenario_id || scenario.created_at != policy.created_at {
            return Err(Error::InvalidRequest(
                "initial policy must belong to and be created with its scenario".to_owned(),
            ));
        }
        let scenario = scenario.clone();
        let policy = policy.clone();
        self.base
            .immediate(|connection| {
                Box::pin(async move {
                    let existing = crate::storage::sqlite::dynamic_sqlite!(
                        select,
                        crate::model::PaymentScenario,
                        crate::storage::EntityQuery::new().eq("id", &scenario.id)
                    )?
                    .build()
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(|error| BaseSqliteRepository::storage_error("find registered payment scenario", error))?;
                    let scenario_inserted = existing.is_none();
                    let registered_scenario = if let Some(row) = existing {
                        Self::decode_row(&row)?
                    } else {
                        crate::storage::sqlite::dynamic_sqlite!(insert, &scenario)?
                            .build()
                            .execute(&mut *connection)
                            .await
                            .map_err(|error| {
                                crate::Error::conflict(format!("payment scenario insert rejected: {error}"))
                            })?;
                        scenario.clone()
                    };
                    if !Self::same_registration(&registered_scenario, &scenario) {
                        return Err(crate::Error::conflict(
                            "scenario registration key is already bound to different content",
                        ));
                    }

                    if let Some(existing) =
                        PolicySqliteRepository::load_policy_connection(connection, &policy.id).await?
                    {
                        if !Self::same_policy_registration(&existing, &policy) {
                            return Err(crate::Error::conflict(
                                "initial payment policy is already bound to different content",
                            ));
                        }
                        return Ok(IdempotentWrite::Replay(ScenarioConfiguration {
                            scenario: registered_scenario,
                            policy: existing,
                        }));
                    }
                    if !scenario_inserted {
                        return Err(crate::Error::conflict(
                            "scenario already exists without the requested atomic initial policy",
                        ));
                    }
                    PolicySqliteRepository::insert_policy_aggregate(connection, &policy).await?;
                    Ok(IdempotentWrite::Inserted(ScenarioConfiguration {
                        scenario: registered_scenario,
                        policy,
                    }))
                })
            })
            .await
    }

    async fn get(&self, scenario_id: &str) -> Result<Option<PaymentScenario>> {
        self.base
            .select_one(crate::storage::EntityQuery::new().eq("id", scenario_id))
            .await
    }

    async fn get_for_client(&self, client_id: &str, scenario_id: &str) -> Result<Option<PaymentScenario>> {
        self.base
            .select_one(
                crate::storage::EntityQuery::new()
                    .eq("client_id", client_id)
                    .eq("id", scenario_id),
            )
            .await
    }

    async fn set_enabled(&self, scenario_id: &str, enabled: bool, updated_at: i64) -> Result<PaymentScenario> {
        self.base
            .entity_update_returning::<PaymentScenario>(
                crate::storage::EntityValues::from([("enabled", enabled.into()), ("updated_at", updated_at.into())]),
                crate::storage::EntityQuery::new().eq("id", scenario_id).compare(
                    "updated_at",
                    crate::storage::EntityComparison::LessOrEqual,
                    updated_at,
                ),
            )
            .await?
            .ok_or_else(|| crate::Error::conflict("payment scenario was not found or update timestamp moved backwards"))
    }

    async fn list_for_client(&self, client_id: &str, page: PageRequest) -> Result<Page<PaymentScenario>> {
        self.base
            .entity_page(
                crate::storage::EntityQuery::new()
                    .eq("client_id", client_id)
                    .order("created_at", false)
                    .order("id", false),
                page,
            )
            .await
    }
}

impl ScenarioSqliteRepository {
    pub(crate) fn new(base: BaseSqliteRepository) -> Self {
        Self { base }
    }

    fn same_registration(stored: &PaymentScenario, requested: &PaymentScenario) -> bool {
        stored.id == requested.id
            && stored.client_id == requested.client_id
            && stored.name == requested.name
            && stored.enabled == requested.enabled
    }

    fn same_policy_registration(stored: &PaymentPolicy, requested: &PaymentPolicy) -> bool {
        stored.id == requested.id
            && stored.scenario_id == requested.scenario_id
            && stored.wallet_ids == requested.wallet_ids
            && stored.protocols == requested.protocols
            && stored.networks == requested.networks
            && stored.assets == requested.assets
            && stored.budget_unit == requested.budget_unit
            && stored.allowed_recipients == requested.allowed_recipients
            && stored.allowed_origins == requested.allowed_origins
            && stored.inbound_limits == requested.inbound_limits
            && stored.outbound_limits == requested.outbound_limits
            && stored.enabled == requested.enabled
    }

    pub(crate) fn decode_row(row: &SqliteRow) -> Result<PaymentScenario> {
        sqlx::FromRow::from_row(row)
            .map_err(|error| BaseSqliteRepository::storage_error("decode scenario entity", error))
    }
}

#[cfg(test)]
#[path = "../../../tests/storage/scenario/sqlite_test.rs"]
mod tests;
