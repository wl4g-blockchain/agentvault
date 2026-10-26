use super::{SystemUnixClock, UnixClock};
use crate::{model::ApprovalExpirationRun, storage::ApprovalStorage, Error, Result};
use std::sync::Arc;

const MAX_EXPIRATION_BATCH_SIZE: u32 = 10_000;

/// Bounded expiration worker for pending approvals and unconsumed grants.
pub struct ApprovalExpirationJob {
    storage: Arc<dyn ApprovalStorage>,
    clock: Arc<dyn UnixClock>,
    batch_size: u32,
}

impl ApprovalExpirationJob {
    /// Builds an expiration worker using the host wall clock.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the batch size is invalid.
    pub fn new(storage: Arc<dyn ApprovalStorage>, batch_size: u32) -> Result<Self> {
        Self::with_clock(storage, Arc::new(SystemUnixClock), batch_size)
    }

    /// Builds an expiration worker with an injected clock.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when the batch size is invalid.
    pub fn with_clock(storage: Arc<dyn ApprovalStorage>, clock: Arc<dyn UnixClock>, batch_size: u32) -> Result<Self> {
        if batch_size == 0 || batch_size > MAX_EXPIRATION_BATCH_SIZE {
            return Err(Error::Config(format!(
                "approval expiry batch_size must be between 1 and {MAX_EXPIRATION_BATCH_SIZE}"
            )));
        }
        Ok(Self {
            storage,
            clock,
            batch_size,
        })
    }

    /// Runs one bounded expiration transaction batch.
    ///
    /// # Errors
    ///
    /// Returns a wall-clock or authoritative storage failure.
    pub async fn run_once(&self) -> Result<ApprovalExpirationRun> {
        let now = self.clock.now()?;
        let expired = self.storage.expire_due(now, self.batch_size).await?;
        Ok(ApprovalExpirationRun {
            expired: u32::try_from(expired.len()).unwrap_or(u32::MAX),
        })
    }
}

#[cfg(test)]
#[path = "../../tests/job/approval_test.rs"]
mod tests;
