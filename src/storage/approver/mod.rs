//! Approver persistence contract.

use crate::{
    model::PaymentScenarioApprover,
    storage::{Page, PageRequest},
    Result,
};
use async_trait::async_trait;

pub mod postgres;
pub mod sqlite;

/// Durable scenario-administrator mappings. Principals originate from trusted
/// authentication and are scoped to exactly one scenario.
#[async_trait]
pub trait IApproverRepository: Send + Sync {
    async fn upsert(&self, approver: &PaymentScenarioApprover) -> Result<PaymentScenarioApprover>;

    async fn get(&self, scenario_id: &str, auth_principal: &str) -> Result<Option<PaymentScenarioApprover>>;

    /// Returns true only when the mapping, scenario, and owning client are all
    /// enabled at the same database snapshot.
    async fn is_authorized(&self, scenario_id: &str, auth_principal: &str) -> Result<bool>;

    async fn set_enabled(
        &self,
        scenario_id: &str,
        auth_principal: &str,
        enabled: bool,
        updated_at: i64,
    ) -> Result<PaymentScenarioApprover>;

    async fn list_for_scenario(&self, scenario_id: &str, page: PageRequest) -> Result<Page<PaymentScenarioApprover>>;
}

#[cfg(test)]
#[path = "../../../tests/storage/approver/mod_test.rs"]
mod tests;

/// Compatibility import for the single repository contract.
pub use IApproverRepository as ApproverStorage;
