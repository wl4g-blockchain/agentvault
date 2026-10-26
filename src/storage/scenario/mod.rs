//! Scenario persistence contract.

use crate::{
    model::{PaymentPolicy, PaymentScenario},
    storage::{IdempotentWrite, Page, PageRequest},
    Result,
};
use async_trait::async_trait;
use serde::Serialize;

pub mod postgres;
pub mod sqlite;

/// Atomically created Scenario configuration. Wallet membership belongs
/// to the initial Policy aggregate rather than being duplicated on the
/// Scenario row.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ScenarioConfiguration {
    pub scenario: PaymentScenario,
    pub policy: PaymentPolicy,
}

#[async_trait]
pub trait IScenarioRepository: Send + Sync {
    /// Registers a scenario idempotently on its stable `id`.
    /// Different content under an existing key must return a conflict.
    async fn register(&self, scenario: &PaymentScenario) -> Result<IdempotentWrite<PaymentScenario>>;

    /// Registers a Scenario and its initial multi-wallet Policy in one
    /// database transaction. Replays must match both aggregates exactly;
    /// partial configuration is never exposed to payment authorization.
    async fn register_with_policy(
        &self,
        scenario: &PaymentScenario,
        policy: &PaymentPolicy,
    ) -> Result<IdempotentWrite<ScenarioConfiguration>>;

    async fn get(&self, scenario_id: &str) -> Result<Option<PaymentScenario>>;

    /// Ownership-aware lookup used after authenticating the calling client.
    async fn get_for_client(&self, client_id: &str, scenario_id: &str) -> Result<Option<PaymentScenario>>;

    async fn set_enabled(&self, scenario_id: &str, enabled: bool, updated_at: i64) -> Result<PaymentScenario>;

    async fn list_for_client(&self, client_id: &str, page: PageRequest) -> Result<Page<PaymentScenario>>;
}

#[cfg(test)]
#[path = "../../../tests/storage/scenario/mod_test.rs"]
mod tests;
