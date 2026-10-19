use std::sync::Arc;

use crate::{
    handler::{WalletBalanceSync, WalletBalanceSyncRun},
    Result,
};

use super::reconciliation::{SystemUnixClock, UnixClock};

/// Cron-facing wrapper for managed-wallet balance synchronization. It owns
/// only trusted time acquisition and delegates all domain work.
pub struct WalletBalanceSyncJob {
    synchronizer: Arc<dyn WalletBalanceSync>,
    clock: Arc<dyn UnixClock>,
}

impl WalletBalanceSyncJob {
    #[must_use]
    pub fn new(synchronizer: Arc<dyn WalletBalanceSync>) -> Self {
        Self::with_clock(synchronizer, Arc::new(SystemUnixClock))
    }

    #[must_use]
    pub fn with_clock(synchronizer: Arc<dyn WalletBalanceSync>, clock: Arc<dyn UnixClock>) -> Self {
        Self { synchronizer, clock }
    }

    /// Executes one bounded observation pass.
    ///
    /// # Errors
    ///
    /// Returns a trusted clock or synchronizer failure.
    pub async fn run_once(&self) -> Result<WalletBalanceSyncRun> {
        self.synchronizer.sync(self.clock.now()?).await
    }
}

#[cfg(test)]
#[path = "../../../tests/handler/job/wallet_balance_sync_test.rs"]
mod tests;
