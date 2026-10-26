//! Policy persistence contract.

use crate::{
    model::PaymentPolicy,
    storage::{Page, PageRequest},
    Result,
};
use async_trait::async_trait;

pub mod postgres;
pub mod sqlite;
pub(crate) mod transactions;

#[async_trait]
pub trait IPolicyRepository: Send + Sync {
    /// Inserts the policy and every `pay_policy_wallet` membership atomically
    /// while locking all wallet rows in stable identifier order. Wallets
    /// without a budget unit are pinned to the policy unit; an existing
    /// different unit is rejected. Enabling a policy that makes any wallet
    /// shared across scenarios requires both wallet-wide hard limits in the
    /// same locked transaction.
    async fn insert(&self, policy: &PaymentPolicy) -> Result<PaymentPolicy>;

    async fn get(&self, policy_id: &str) -> Result<Option<PaymentPolicy>>;

    /// Replaces mutable policy fields using `expected_updated_at` as an
    /// optimistic concurrency token while locking/revalidating every bound
    /// wallet. Wallet membership, scenario, and budget unit are immutable;
    /// disabling a policy never clears wallet units.
    async fn update(&self, policy: &PaymentPolicy, expected_updated_at: i64) -> Result<PaymentPolicy>;

    async fn list_for_scenario(&self, scenario_id: &str, page: PageRequest) -> Result<Page<PaymentPolicy>>;

    async fn list_for_wallet(&self, wallet_id: &str, page: PageRequest) -> Result<Page<PaymentPolicy>>;

    /// Returns whether an enabled policy currently binds the wallet to
    /// the scenario. This authority check must not depend on pagination.
    async fn has_enabled_wallet_binding(&self, scenario_id: &str, wallet_id: &str) -> Result<bool>;
}

#[cfg(test)]
#[path = "../../../tests/storage/policy/mod_test.rs"]
mod tests;

mod validation {
    use crate::{model::PaymentPolicy, Error, Result};

    pub(crate) struct PolicyStorageRules;
    impl PolicyStorageRules {
        pub(crate) fn validate_policy(policy: &PaymentPolicy) -> Result<()> {
            policy
                .validate()
                .map_err(|field| Error::InvalidRequest(format!("invalid payment policy {field}")))
        }
    }
}
pub(crate) use validation::*;
