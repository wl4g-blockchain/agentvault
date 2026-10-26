use super::{lifecycle::RuntimeComponent, lifecycle::RuntimeResource, lifecycle::Shutdown};
use crate::{
    apm::AgentVaultMetrics, apm::ReadinessProbe, cache::ICache, config::AppConfig, job::ApprovalExpirationJob,
    job::ReconciliationJob, job::WalletBalanceSyncJob, model::AssetIdentity, payment::MerchantEvidenceRecorder,
    payment::PaymentEvidenceRouter, payment::ReconciliationConfig as JobReconciliationConfig,
    payment::ReconciliationEvidenceSource, payment::SettlementCoordinator, payment::WalletBalanceSource,
    payment::WalletBalanceSynchronizer, storage::IApprovalRepository, storage::IIntentRepository, storage::IRepository,
    storage::IWalletBalanceRepository, storage::IWalletRepository,
};
use anyhow::Context as _;
use async_trait::async_trait;
use std::sync::Arc;

pub(crate) struct StorageReadinessProbe {
    pub(crate) storage: Arc<dyn IRepository>,
}

#[async_trait]
impl ReadinessProbe for StorageReadinessProbe {
    fn name(&self) -> &'static str {
        "authoritative SQL storage"
    }

    async fn check(&self) -> anyhow::Result<()> {
        self.storage.ping().await.context("ping authoritative SQL storage")
    }
}

pub(crate) struct PaymentRuntimeResources {
    pub(crate) cache: Arc<dyn ICache>,
    pub(crate) storage: Arc<dyn IRepository>,
}

#[async_trait]
impl RuntimeResource for PaymentRuntimeResources {
    fn name(&self) -> &'static str {
        "payment providers"
    }

    async fn close(self: Box<Self>) -> anyhow::Result<()> {
        let cache_result = self.cache.close().await.context("close cache provider");
        let storage_result = self.storage.close().await.context("close storage provider");
        cache_result.and(storage_result)
    }
}

/// Keeps the selected best-effort cache adapter live without making payment
/// correctness depend on it. SQL remains authoritative if a later ping fails.
pub(crate) struct CacheHealthComponent {
    pub(crate) cache: Arc<dyn ICache>,
    pub(crate) interval: std::time::Duration,
}

#[async_trait]
impl RuntimeComponent for CacheHealthComponent {
    fn name(&self) -> &'static str {
        "non-authoritative cache"
    }

    async fn run(self: Box<Self>, mut shutdown: Shutdown) -> anyhow::Result<()> {
        let mut interval = tokio::time::interval(self.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                _ = interval.tick() => {
                    if let Err(error) = self.cache.ping().await {
                        tracing::warn!(%error, "cache is unavailable; continuing with authoritative SQL");
                    }
                }
            }
        }
    }
}

pub(crate) struct ApprovalExpirationComponent {
    pub(crate) job: ApprovalExpirationJob,
    pub(crate) interval: std::time::Duration,
}

#[async_trait]
impl RuntimeComponent for ApprovalExpirationComponent {
    fn name(&self) -> &'static str {
        "payment approval expiration"
    }

    async fn run(self: Box<Self>, mut shutdown: Shutdown) -> anyhow::Result<()> {
        let mut interval = tokio::time::interval(self.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                _ = interval.tick() => {
                    let run = self.job.run_once().await.context("run approval expiration batch")?;
                    tracing::debug!(expired = run.expired, "approval expiration batch completed");
                }
            }
        }
    }
}

pub(crate) struct ReconciliationComponent {
    pub(crate) job: ReconciliationJob,
    pub(crate) interval: std::time::Duration,
}

pub(crate) struct WalletBalanceSyncComponent {
    pub(crate) job: WalletBalanceSyncJob,
    pub(crate) interval: std::time::Duration,
}

#[async_trait]
impl RuntimeComponent for WalletBalanceSyncComponent {
    fn name(&self) -> &'static str {
        "managed-wallet balance synchronization"
    }

    async fn run(self: Box<Self>, mut shutdown: Shutdown) -> anyhow::Result<()> {
        let mut interval = tokio::time::interval(self.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                _ = interval.tick() => {
                    let run = self.job.run_once().await.context("run managed-wallet balance synchronization")?;
                    tracing::info!(
                        event.name = "agentvault.wallet.balance_sync_completed",
                        scanned = run.scanned,
                        observed = run.observed,
                        skipped = run.skipped,
                        failed = run.failed,
                        "managed-wallet balance synchronization completed"
                    );
                }
            }
        }
    }
}

#[async_trait]
impl RuntimeComponent for ReconciliationComponent {
    fn name(&self) -> &'static str {
        "payment reconciliation"
    }

    async fn run(self: Box<Self>, mut shutdown: Shutdown) -> anyhow::Result<()> {
        let mut interval = tokio::time::interval(self.interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                _ = interval.tick() => {
                    let run = self.job.run_once().await.context("run payment reconciliation batch")?;
                    tracing::debug!(
                        claimed = run.claimed,
                        settled = run.settled,
                        verified = run.verified,
                        expired = run.expired,
                        pending = run.pending,
                        unknown = run.unknown,
                        unavailable = run.unavailable,
                        provider_errors = run.provider_errors,
                        skipped = run.skipped,
                        "payment reconciliation batch completed"
                    );
                }
            }
        }
    }
}

pub(crate) struct RuntimeJobFactory;
impl RuntimeJobFactory {
    pub(crate) fn build_reconciliation(
        config: &AppConfig,
        payments: Arc<dyn IIntentRepository>,
        settlement: Arc<SettlementCoordinator>,
        outbound_evidence: Arc<MerchantEvidenceRecorder>,
    ) -> anyhow::Result<Option<ReconciliationComponent>> {
        let settings = &config.agentvault.reconciliation;
        if !settings.enabled {
            return Ok(None);
        }
        let provider: Arc<dyn ReconciliationEvidenceSource> = settlement;
        let evidence: Arc<dyn ReconciliationEvidenceSource> =
            Arc::new(PaymentEvidenceRouter::new(provider, outbound_evidence));
        Ok(Some(ReconciliationComponent {
            job: ReconciliationJob::new(
                payments,
                evidence,
                JobReconciliationConfig {
                    worker_id: RuntimeJobFactory::reconciliation_worker_id(&config.server.service_name),
                    batch_size: u32::try_from(settings.batch_size).context("reconciliation batch size exceeds u32")?,
                    lease_duration: settings.lease_ttl,
                    initial_backoff: settings.initial_backoff,
                    max_backoff: settings.max_backoff,
                },
            )
            .context("build reconciliation job")?,
            interval: settings.interval,
        }))
    }

    pub(crate) fn build_wallet_balance_sync(
        config: &AppConfig,
        wallets: Arc<dyn IWalletRepository>,
        wallet_balances: Arc<dyn IWalletBalanceRepository>,
        sources: Arc<dyn WalletBalanceSource>,
        metrics: AgentVaultMetrics,
    ) -> anyhow::Result<Option<WalletBalanceSyncComponent>> {
        let settings = &config.agentvault.wallet_balance_sync;
        if !settings.enabled {
            return Ok(None);
        }
        let synchronizer = Arc::new(
            WalletBalanceSynchronizer::new(
                wallets,
                wallet_balances,
                sources,
                RuntimeJobFactory::payer_balance_assets(config),
                metrics,
                u32::try_from(settings.batch_size).context("wallet balance sync batch size exceeds u32")?,
            )
            .context("build managed-wallet balance synchronizer")?,
        );
        Ok(Some(WalletBalanceSyncComponent {
            job: WalletBalanceSyncJob::new(synchronizer),
            interval: settings.interval,
        }))
    }

    pub(crate) fn payer_balance_assets(config: &AppConfig) -> Vec<AssetIdentity> {
        let mut assets = Vec::new();
        if config.agentvault.payer.x402.enabled {
            assets.extend(config.agentvault.payer.x402.assets.iter().map(|asset| AssetIdentity {
                network: asset.network.clone(),
                asset: asset.symbol.clone(),
                token_contract: Some(asset.contract.clone()),
                decimals: asset.decimals,
            }));
        }
        if config.agentvault.payer.mpp.enabled {
            assets.extend(config.agentvault.payer.mpp.assets.iter().map(|asset| AssetIdentity {
                network: asset.network.clone(),
                asset: asset.symbol.clone(),
                token_contract: Some(asset.currency.clone()),
                decimals: asset.decimals,
            }));
        }
        assets
    }

    pub(crate) fn reconciliation_worker_id(service_name: &str) -> String {
        crate::utils::runtime_identity::RuntimeIdentity::unique(&format!("{service_name}-reconciliation"))
    }

    pub(crate) fn build_approval_expiration(
        config: &AppConfig,
        approvals: Arc<dyn IApprovalRepository>,
    ) -> anyhow::Result<ApprovalExpirationComponent> {
        let settings = &config.agentvault.approval;
        Ok(ApprovalExpirationComponent {
            job: ApprovalExpirationJob::new(
                approvals,
                u32::try_from(settings.expiry_batch_size).context("approval expiry batch size exceeds u32")?,
            )
            .context("build approval expiration job")?,
            interval: settings.expiry_interval,
        })
    }
}

#[cfg(test)]
#[path = "../../tests/cmd/payment_jobs_test.rs"]
mod tests;
