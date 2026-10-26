//! Ledger domain service. External integrations belong to dedicated components.
use crate::{
    model::PaymentLedgerEntry,
    storage::{ILedgerRepository, IdempotentWrite, LedgerQuery, LedgerWrite, Page},
    Result,
};
use std::sync::Arc;

pub struct LedgerHandler {
    repository: Arc<dyn ILedgerRepository>,
}
impl LedgerHandler {
    #[must_use]
    pub fn new(repository: Arc<dyn ILedgerRepository>) -> Self {
        Self { repository }
    }

    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn append(&self, entry: &PaymentLedgerEntry) -> Result<IdempotentWrite<PaymentLedgerEntry>> {
        self.repository.append(entry).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn append_batch(&self, write: LedgerWrite<'_>) -> Result<Vec<IdempotentWrite<PaymentLedgerEntry>>> {
        self.repository.append_batch(write).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn get(&self, id: &str) -> Result<Option<PaymentLedgerEntry>> {
        self.repository.get(id).await
    }
    /// # Errors
    /// Returns validation, authority or persistence errors.
    pub async fn list(&self, query: &LedgerQuery) -> Result<Page<PaymentLedgerEntry>> {
        self.repository.list(query).await
    }
}

#[cfg(test)]
#[path = "../../tests/handler/ledger_test.rs"]
mod tests;
