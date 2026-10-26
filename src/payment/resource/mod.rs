//! Idempotent console resource import for payment identities and policy.
//!
//! Resource files contain only public metadata and environment-variable names.
//! Local private keys are resolved at execution time, zeroized in memory, and
//! persisted only through the versioned SQL envelope-encryption repositories.

use std::{
    collections::BTreeSet,
    fs::{self, OpenOptions},
    io::Read as _,
    os::unix::fs::OpenOptionsExt as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use serde::Deserialize;
use serde_yaml::{Mapping, Value};
use zeroize::Zeroizing;

use crate::config::AppConfig;
use crate::model::resource::ApproverSpec;
use crate::model::resource::ClientSpec;
use crate::model::resource::ImportBatch;
use crate::model::resource::PolicySpec;
use crate::model::resource::PriceSpec;
use crate::model::resource::ResourceEnvelope;
use crate::model::resource::ResourceKind;
use crate::model::resource::ScenarioSpec;
use crate::model::resource::WalletSource;
use crate::model::resource::WalletSpec;
use crate::model::wallet::Wallet;
use crate::model::Amount;
use crate::model::ImportReport;
use crate::model::PaymentClient;
use crate::model::PaymentPolicy;
use crate::model::PaymentPrice;
use crate::model::PaymentScenario;
use crate::storage::ApproverStorage;
use crate::storage::ClientStorage;
use crate::storage::PolicyStorage;
use crate::storage::PriceStorage;
use crate::storage::ScenarioStorage;
use crate::storage::StorageFactory;
use crate::storage::StoragePorts;
use crate::storage::WalletCreationConfig;
use crate::storage::WalletCustody;
use crate::storage::WalletKeyManager;
use crate::storage::WalletStorage;
use crate::utils::crypto;
use crate::utils::secret::SecretReader;
use crate::Error;
use crate::Result;

const CONSOLE_VERSION: &str = "core.agentvault.io/v1";
const MAX_RESOURCE_FILE_BYTES: u64 = 1024 * 1024;
const MAX_RESOURCE_FILE_BODY_BYTES: usize = 1024 * 1024;
const MAX_RESOURCE_FILES: usize = 1_000;
const MAX_IMPORT_DEPTH: usize = 8;

pub(crate) struct ImportPorts {
    clients: Arc<dyn ClientStorage>,
    scenarios: Arc<dyn ScenarioStorage>,
    policies: Arc<dyn PolicyStorage>,
    offers: Arc<dyn PriceStorage>,
    approvers: Arc<dyn ApproverStorage>,
    wallets: Arc<dyn WalletStorage>,
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
#[path = "../../../tests/payment/resource/mod_test.rs"]
mod tests;
