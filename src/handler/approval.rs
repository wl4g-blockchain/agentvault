use crate::{
    model::ApprovalRequest,
    storage::{IApprovalRepository, Page, PageRequest},
    Result,
};
use std::sync::Arc;

/// Approval-row application service. Grant issuance and cross-intent transactions
/// are coordinated by the dedicated payment approval workflow.
pub struct ApprovalHandler {
    repository: Arc<dyn IApprovalRepository>,
}

impl ApprovalHandler {
    #[must_use]
    pub fn new(repository: Arc<dyn IApprovalRepository>) -> Self {
        Self { repository }
    }

    /// # Errors
    /// Returns a repository error.
    pub async fn get(&self, id: &str) -> Result<Option<ApprovalRequest>> {
        self.repository.get(id).await
    }

    /// # Errors
    /// Returns a repository error.
    pub async fn list_pending(&self, scenario_id: &str, now: i64, page: PageRequest) -> Result<Page<ApprovalRequest>> {
        self.repository.list_pending(scenario_id, now, page).await
    }
}

#[cfg(test)]
#[path = "../../tests/handler/approval_test.rs"]
mod tests;
