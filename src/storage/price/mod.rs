//! Price persistence contract.

use crate::{
    model::PaymentPrice,
    storage::{Page, PageRequest},
    Result,
};
use async_trait::async_trait;

pub mod postgres;
pub mod sqlite;

#[async_trait]
pub trait IPriceRepository: Send + Sync {
    /// Inserts an offer only when `(policy_id, wallet_id)` is an explicit
    /// `pay_policy_wallet` membership and the policy authorizes inbound flow
    /// for the offer's scenario and budget unit.
    async fn insert(&self, offer: &PaymentPrice) -> Result<PaymentPrice>;

    async fn get(&self, offer_id: &str) -> Result<Option<PaymentPrice>>;

    /// Resolves only the enabled offer owned by the authenticated scenario.
    /// Storage must enforce at most one active offer per `(scenario, resource)`;
    /// the row's protocol list represents alternate adapters for the same
    /// fixed price and recipient. Ambiguous rows are an invariant error, never
    /// a `LIMIT 1` selection.
    async fn resolve(&self, scenario_id: &str, resource: &str, protocol: &str) -> Result<Option<PaymentPrice>>;

    /// Resolves the single enabled offer for a scenario resource before
    /// the caller has selected one of the offer's supported protocols.
    /// This must be a direct authority query; callers must not infer the
    /// result from an arbitrary page of catalog rows.
    async fn resolve_resource(&self, scenario_id: &str, resource: &str) -> Result<Option<PaymentPrice>>;

    /// Replaces pricing or lifecycle fields with optimistic concurrency.
    async fn update(&self, offer: &PaymentPrice, expected_updated_at: i64) -> Result<PaymentPrice>;

    /// Deletes an unused price at the expected revision. Intent references are
    /// protected by the database foreign key and must never be cascaded away.
    async fn delete(&self, price_id: &str, expected_updated_at: i64) -> Result<()>;

    async fn list_for_scenario(&self, scenario_id: &str, page: PageRequest) -> Result<Page<PaymentPrice>>;
}

#[cfg(test)]
#[path = "../../../tests/storage/price/mod_test.rs"]
mod tests;

/// Compatibility import for the single repository contract.
pub use IPriceRepository as PriceStorage;
