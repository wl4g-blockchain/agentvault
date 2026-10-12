use std::sync::Arc;

use crate::{model::wallet::Wallet, observability::AgentVaultMetrics, EncryptedFileStore, Result};

/// Public wallet metadata operations. Private key material never leaves the store.
pub struct WalletService {
    store: Arc<EncryptedFileStore>,
    metrics: AgentVaultMetrics,
}

impl WalletService {
    #[must_use]
    pub fn new(store: Arc<EncryptedFileStore>, metrics: AgentVaultMetrics) -> Self {
        Self { store, metrics }
    }

    /// Creates a wallet with a fresh private key.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or duplicate ID or storage failure.
    pub fn create(&self, id: &str) -> Result<Wallet> {
        self.observe("create", self.store.generate(id).map(Into::into))
    }

    /// Gets public metadata for one wallet.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or missing ID or storage failure.
    pub fn get(&self, id: &str) -> Result<Wallet> {
        self.observe("get", self.store.get(id).map(Into::into))
    }

    /// Lists all wallets by ID.
    ///
    /// # Errors
    ///
    /// Returns an error when stored metadata cannot be read safely.
    pub fn list(&self) -> Result<Vec<Wallet>> {
        self.observe(
            "list",
            self.store
                .list()
                .map(|wallets| wallets.into_iter().map(Into::into).collect()),
        )
    }

    /// Atomically rotates a wallet to a newly generated private key.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or missing ID or storage failure.
    pub fn rotate(&self, id: &str) -> Result<Wallet> {
        self.observe("rotate", self.store.rotate(id).map(Into::into))
    }

    /// Deletes one wallet.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or missing ID or storage failure.
    pub fn delete(&self, id: &str) -> Result<()> {
        self.observe("delete", self.store.delete(id))
    }

    fn observe<T>(&self, operation: &str, result: Result<T>) -> Result<T> {
        self.metrics.record_wallet(operation, result.is_ok());
        result
    }
}
