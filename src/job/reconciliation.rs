use crate::{
    model::ReconciliationRun, model::WalletBalanceSyncRun, payment::PaymentReconciler, payment::ReconciliationConfig,
    payment::ReconciliationEvidenceSource, payment::WalletBalanceSync, storage::IIntentRepository,
    utils::clock::SystemUnixClock, utils::clock::UnixClock, Result,
};
use std::sync::Arc;

/// Scheduler adapter; financial transitions and recovery belong to `PaymentReconciler`.
pub struct ReconciliationJob {
    processor: PaymentReconciler,
}
impl ReconciliationJob {
    /// # Errors
    /// Returns invalid worker configuration.
    pub fn new(
        storage: Arc<dyn IIntentRepository>,
        evidence: Arc<dyn ReconciliationEvidenceSource>,
        config: ReconciliationConfig,
    ) -> Result<Self> {
        Ok(Self {
            processor: PaymentReconciler::new(storage, evidence, config)?,
        })
    }
    /// # Errors
    /// Returns invalid worker configuration.
    pub fn with_clock(
        storage: Arc<dyn IIntentRepository>,
        evidence: Arc<dyn ReconciliationEvidenceSource>,
        clock: Arc<dyn UnixClock>,
        config: ReconciliationConfig,
    ) -> Result<Self> {
        Ok(Self {
            processor: PaymentReconciler::with_clock(storage, evidence, clock, config)?,
        })
    }
    /// # Errors
    /// Propagates a reconciliation pass failure.
    pub async fn run_once(&self) -> Result<ReconciliationRun> {
        self.processor.run_once().await
    }
}

/// Cron-facing wrapper that owns trusted time acquisition and delegates the
/// balance observation pass.
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
    /// Returns a trusted-clock or synchronizer failure.
    pub async fn run_once(&self) -> Result<WalletBalanceSyncRun> {
        self.synchronizer.sync(self.clock.now()?).await
    }
}

#[cfg(test)]
#[path = "../../tests/job/reconciliation_test.rs"]
mod tests;
