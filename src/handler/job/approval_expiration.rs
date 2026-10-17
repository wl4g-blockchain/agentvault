use std::sync::Arc;

use crate::{storage::ApprovalStorage, Error, Result};

use super::reconciliation::{SystemUnixClock, UnixClock};

const MAX_BATCH_SIZE: u32 = 10_000;

/// Summary for one bounded approval-expiration pass.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ApprovalExpirationRun {
    pub expired: u32,
}

/// Expires pending requests and approved-but-unconsumed grants through the
/// storage port's authoritative intent+approval transaction.
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
    /// Returns a configuration error when `batch_size` is zero or unbounded.
    pub fn new(storage: Arc<dyn ApprovalStorage>, batch_size: u32) -> Result<Self> {
        Self::with_clock(storage, Arc::new(SystemUnixClock), batch_size)
    }

    /// Builds an expiration worker with an injected clock.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when `batch_size` is zero or unbounded.
    pub fn with_clock(storage: Arc<dyn ApprovalStorage>, clock: Arc<dyn UnixClock>, batch_size: u32) -> Result<Self> {
        if batch_size == 0 || batch_size > MAX_BATCH_SIZE {
            return Err(Error::Config(format!(
                "approval expiry batch_size must be between 1 and {MAX_BATCH_SIZE}"
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
#[path = "../../../tests/handler/job/approval_expiration_test.rs"]
mod tests;
