//! Idempotent console resource import for payment identities and policy.
//!
//! Resource files contain only public metadata and environment-variable names.
//! Local private keys are resolved at execution time, zeroized in memory, and
//! persisted only through the versioned SQL envelope-encryption repositories.

use std::{
    collections::BTreeSet, fs, fs::OpenOptions, io::Read as _, os::unix::fs::OpenOptionsExt as _, path::Path,
    path::PathBuf, sync::Arc,
};

use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use zeroize::Zeroizing;

use crate::{
    config::AppConfig, model::resource::ApproverSpec, model::resource::ClientSpec, model::resource::ImportBatch,
    model::resource::PolicySpec, model::resource::PriceSpec, model::resource::ResourceEnvelope,
    model::resource::ResourceKind, model::resource::ScenarioSpec, model::resource::WalletSource,
    model::resource::WalletSpec, model::wallet::Wallet, model::Amount, model::ImportReport, model::PaymentClient,
    model::PaymentPolicy, model::PaymentPrice, model::PaymentScenario, storage::IApproverRepository,
    storage::IClientRepository, storage::IPolicyRepository, storage::IPriceRepository, storage::IScenarioRepository,
    storage::IWalletRepository, storage::StorageFactory, storage::StoragePorts, storage::WalletCreationConfig,
    storage::WalletCustody, storage::WalletKeyManager, util::crypto, util::secret::SecretReader, Error, Result,
};

const CONSOLE_VERSION: &str = "core.agentvault.io/v1";
const MAX_RESOURCE_FILE_BYTES: u64 = 1024 * 1024;
const MAX_RESOURCE_FILE_BODY_BYTES: usize = 1024 * 1024;
const MAX_RESOURCE_FILES: usize = 1_000;
const MAX_IMPORT_DEPTH: usize = 8;

pub(crate) struct ImportPorts {
    clients: Arc<dyn IClientRepository>,
    scenarios: Arc<dyn IScenarioRepository>,
    policies: Arc<dyn IPolicyRepository>,
    prices: Arc<dyn IPriceRepository>,
    approvers: Arc<dyn IApproverRepository>,
    wallets: Arc<dyn IWalletRepository>,
    local_keys: Arc<dyn WalletKeyManager>,
}

#[derive(Clone, Copy)]
pub(crate) enum ImportScope {
    All,
    WalletsOnly,
}

mod apply;
mod reader;
mod validation;

pub struct ResourceImporter;

impl ResourceImporter {
    /// Imports canonical `AgentVault` resource files into the configured SQL
    /// authority.
    ///
    /// Existing records are reused only when their immutable and effective
    /// configuration exactly matches the resource. Drift fails closed instead of
    /// silently mutating payment authority.
    ///
    /// # Errors
    ///
    /// Fails for unsafe input paths, invalid or duplicate resources, unavailable
    /// environment secrets, configuration drift, or any SQL transaction failure.
    pub async fn import_from_paths(config: &AppConfig, paths: &[PathBuf]) -> Result<ImportReport> {
        ResourceImporter::import_from_paths_with_scope(config, paths, ImportScope::All).await
    }

    /// Creates wallets from canonical resource files without accepting any other
    /// resource kind. This is the sole console wallet mutation: existing wallet
    /// metadata or custody keys cannot be updated, rotated, deleted, or exported.
    ///
    /// # Errors
    ///
    /// Returns the same validation, storage, and custody errors as
    /// [`import_from_paths`], plus an error when a path contains a non-wallet
    /// resource or contains no wallet resource.
    pub async fn create_wallets_from_paths(config: &AppConfig, paths: &[PathBuf]) -> Result<ImportReport> {
        ResourceImporter::import_from_paths_with_scope(config, paths, ImportScope::WalletsOnly).await
    }

    async fn import_from_paths_with_scope(
        config: &AppConfig,
        paths: &[PathBuf],
        scope: ImportScope,
    ) -> Result<ImportReport> {
        let mut batch = ResourceImporter::read_resources(paths)?;
        ResourceImporter::normalize_batch(&mut batch);
        ResourceImporter::validate_batch(&batch)?;
        if matches!(scope, ImportScope::WalletsOnly) && !batch.contains_only_wallets() {
            return Err(Error::InvalidRequest(
                "wallet create accepts one or more Wallet resources only".to_owned(),
            ));
        }
        let factory = StorageFactory::new();
        let storage = factory.create(&config.storage).await?;
        let result = async {
            let local_keys = Arc::clone(&storage).wallet_key_manager(&config.agentvault.key_management)?;
            let ports = ResourceImporter::import_ports(Arc::clone(&storage).ports(), local_keys);
            ResourceImporter::apply_batch(&ports, batch).await
        }
        .await;
        let close = storage.close().await;
        match result {
            Err(error) => Err(error),
            Ok(report) => {
                close?;
                Ok(report)
            }
        }
    }
}

#[cfg(test)]
#[path = "../../../tests/storage/resource/mod_test.rs"]
mod tests;
