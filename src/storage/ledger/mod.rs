//! Ledger persistence contract.

use crate::{
    model::{PaymentDirection, PaymentLedgerEntry},
    storage::{EntityComparison, EntityQuery, EntityValue, IdempotentWrite, OutboxMessage, Page, PageRequest},
    Result,
};
use async_trait::async_trait;

pub mod postgres;
pub mod sqlite;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LedgerQuery {
    pub scenario_id: Option<String>,
    pub wallet_id: Option<String>,
    pub intent_id: Option<String>,
    pub direction: Option<PaymentDirection>,
    pub network: Option<String>,
    pub asset: Option<String>,
    pub from_effective_at: Option<i64>,
    pub to_effective_at: Option<i64>,
    pub page: PageRequest,
}

/// One append-only ledger transaction. Outbox messages are committed in the
/// same database transaction as all entries.
pub struct LedgerWrite<'a> {
    pub entries: &'a [PaymentLedgerEntry],
    pub outbox: &'a [OutboxMessage],
}

#[async_trait]
pub trait ILedgerRepository: Send + Sync {
    /// Appends one confirmed financial fact. The same `(wallet_id,
    /// posting_key)` and identical content is a replay; different content is a
    /// conflict. Multiple facts sharing one transaction/reference require a
    /// trusted, canonical `effect_ref`; without one, storage treats the
    /// evidence as a single fact. A reversal transaction must lock its original row, apply
    /// [`PaymentLedgerEntry::validate_reversal_of`], and reject a second
    /// reversal of the same original.
    async fn append(&self, entry: &PaymentLedgerEntry) -> Result<IdempotentWrite<PaymentLedgerEntry>>;

    /// Atomically appends a transfer pair, reversal batch, or deposit batch and
    /// its externally visible notifications. Every reversal has the same
    /// exact-wallet-fact and at-most-once requirements as [`Self::append`].
    async fn append_batch(&self, write: LedgerWrite<'_>) -> Result<Vec<IdempotentWrite<PaymentLedgerEntry>>>;

    async fn get(&self, ledger_id: &str) -> Result<Option<PaymentLedgerEntry>>;

    async fn find_posting(&self, wallet_id: &str, posting_key: &str) -> Result<Option<PaymentLedgerEntry>>;

    async fn list(&self, query: &LedgerQuery) -> Result<Page<PaymentLedgerEntry>>;
}

impl LedgerQuery {
    pub(crate) fn entity_query(&self) -> EntityQuery {
        let mut query = EntityQuery::new();
        if let Some(value) = &self.scenario_id {
            query = query.compare("scenario_id", EntityComparison::Equal, EntityValue::from(value));
        }
        if let Some(value) = &self.wallet_id {
            query = query.compare("wallet_id", EntityComparison::Equal, EntityValue::from(value));
        }
        if let Some(value) = &self.intent_id {
            query = query.compare("intent_id", EntityComparison::Equal, EntityValue::from(value));
        }
        if let Some(value) = &self.direction {
            query = query.compare("direction", EntityComparison::Equal, EntityValue::from(value));
        }
        if let Some(value) = &self.network {
            query = query.compare("network", EntityComparison::Equal, EntityValue::from(value));
        }
        if let Some(value) = &self.asset {
            query = query.compare("asset", EntityComparison::Equal, EntityValue::from(value));
        }
        if let Some(value) = &self.from_effective_at {
            query = query.compare(
                "effective_at",
                EntityComparison::GreaterOrEqual,
                EntityValue::from(value),
            );
        }
        if let Some(value) = &self.to_effective_at {
            query = query.compare("effective_at", EntityComparison::LessOrEqual, EntityValue::from(value));
        }
        query
    }
}

#[cfg(test)]
#[path = "../../../tests/storage/ledger/mod_test.rs"]
mod tests;

/// Compatibility import for the single repository contract.
pub use ILedgerRepository as LedgerStorage;
