//! Wallet persistence contract.

use crate::{
    model::Amount, model::AssetIdentity, model::BudgetUnit, model::Wallet, storage::Page, storage::PageRequest,
    storage::WalletPaymentSigner, Result,
};
use async_trait::async_trait;

pub(crate) mod r#macro;
pub mod postgres;
pub mod sqlite;

/// Versioned local-wallet custody operations backed by the same authoritative
/// SQL component. Raw private-key material never leaves this boundary.
#[async_trait]
pub trait WalletKeyManager: WalletPaymentSigner {
    async fn create_local(
        &self,
        wallet_id: &str,
        algorithm: &str,
        private_key: &[u8],
        config: &WalletCreationConfig,
        created_at: i64,
    ) -> Result<Wallet>;

    async fn rotate(&self, wallet_id: &str, private_key: &[u8], activated_at: i64) -> Result<Wallet>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WalletCustody {
    Local,
    External,
}

/// Initial lifecycle and aggregate-limit metadata committed with a new wallet.
/// Keeping this in the wallet creation transaction prevents import or API
/// crashes from leaving an unprovable half-configured authority record.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WalletCreationConfig {
    pub enabled: bool,
    pub budget_unit: Option<BudgetUnit>,
    pub max_per_payment: Option<Amount>,
    pub max_per_day: Option<Amount>,
}

impl WalletCreationConfig {
    /// Validates initial wallet limit metadata.
    ///
    /// # Errors
    ///
    /// Returns the invalid metadata field.
    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        if let Some(unit) = &self.budget_unit {
            unit.validate().map_err(|_| "budget_unit")?;
        } else if self.max_per_payment.is_some() || self.max_per_day.is_some() {
            return Err("budget_unit");
        }
        if self.max_per_payment.is_some_and(Amount::is_zero) {
            return Err("max_per_payment");
        }
        if self.max_per_day.is_some_and(Amount::is_zero)
            || self
                .max_per_payment
                .zip(self.max_per_day)
                .is_some_and(|(payment, day)| payment > day)
        {
            return Err("max_per_day");
        }
        Ok(())
    }
}

/// Payment-facing wallet lifecycle and aggregate-limit metadata. Public wallet
/// metadata alone is intentionally insufficient to authorize money use.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentWallet {
    pub wallet: Wallet,
    pub custody: WalletCustody,
    pub enabled: bool,
    /// Current immutable key version for local custody. External recipients
    /// and local wallets without an active signing key use `None`.
    pub active_key_version: Option<u32>,
    /// Denomination shared by every inbound or outbound policy bound to this
    /// wallet. `None` is valid only before the first policy association and
    /// when no wallet-level aggregate limits exist.
    pub budget_unit: Option<BudgetUnit>,
    pub max_per_payment: Option<Amount>,
    pub max_per_day: Option<Amount>,
    pub updated_at: i64,
}

impl PaymentWallet {
    /// Validates and canonicalizes payment-facing wallet metadata loaded from
    /// storage before it participates in policy or price evaluation.
    ///
    /// # Errors
    ///
    /// Returns `address` for malformed EVM wallet metadata.
    pub fn canonicalized(mut self) -> std::result::Result<Self, &'static str> {
        self.wallet = self.wallet.canonicalized()?;
        self.validate_payment_metadata()?;
        Ok(self)
    }

    /// Also validates the recipient address family required by the price's
    /// payment network. In particular, external custody does not make an
    /// opaque address valid on an EIP-155 network.
    ///
    /// # Errors
    ///
    /// Returns `address` when the wallet address is invalid for `network`.
    pub fn canonicalized_for_payment_network(mut self, network: &str) -> std::result::Result<Self, &'static str> {
        self.wallet = self.wallet.canonicalized_for_payment_network(network)?;
        self.validate_payment_metadata()?;
        Ok(self)
    }

    /// Validates payment lifecycle and wallet-wide scalar-limit metadata.
    ///
    /// # Errors
    ///
    /// Fails when an external wallet claims a local key version, a key version
    /// is zero, limits have no unit, a limit is zero, or the daily limit is
    /// smaller than the per-payment limit.
    pub fn validate_payment_metadata(&self) -> std::result::Result<(), &'static str> {
        if (matches!(self.custody, WalletCustody::External) && self.active_key_version.is_some())
            || self.active_key_version == Some(0)
        {
            return Err("active_key_version");
        }
        if let Some(unit) = &self.budget_unit {
            unit.validate().map_err(|_| "budget_unit")?;
        } else if self.max_per_payment.is_some() || self.max_per_day.is_some() {
            return Err("budget_unit");
        }
        if self.max_per_payment.is_some_and(Amount::is_zero) {
            return Err("max_per_payment");
        }
        if self.max_per_day.is_some_and(Amount::is_zero)
            || self
                .max_per_payment
                .zip(self.max_per_day)
                .is_some_and(|(per_payment, per_day)| per_payment > per_day)
        {
            return Err("max_per_day");
        }
        Ok(())
    }

    /// Returns whether this wallet may start a new outbound payment.
    #[must_use]
    pub const fn signing_ready(&self) -> bool {
        self.enabled && matches!(self.custody, WalletCustody::Local) && self.active_key_version.is_some()
    }

    /// Returns whether the built-in local signer can use this wallet on
    /// the requested payment network. EIP-155 payments require both a
    /// canonical EVM address and the secp256k1/EVM algorithm family.
    #[must_use]
    pub fn signing_ready_for_payment_network(&self, network: &str) -> bool {
        let compatible = if crate::model::wallet::is_evm_payment_network(network) {
            crate::model::wallet::is_evm_wallet_algorithm(&self.wallet.algorithm)
        } else if crate::model::wallet::is_solana_payment_network(network) {
            crate::model::wallet::is_solana_wallet_algorithm(&self.wallet.algorithm)
        } else {
            false
        };
        self.signing_ready() && compatible && self.clone().canonicalized_for_payment_network(network).is_ok()
    }
}

#[async_trait]
pub trait IWalletRepository: Send + Sync {
    /// Creates local wallet metadata and its first encrypted key version in one
    /// transaction. Implementations must persist [`Wallet::canonicalized`]
    /// metadata and enforce address uniqueness transactionally.
    async fn create_local(&self, wallet: &Wallet, key: &EncryptedWalletKeyVersion) -> Result<Wallet>;

    /// Creates the local wallet, encrypted key, lifecycle flag, and optional
    /// aggregate limits atomically.
    async fn create_local_configured(
        &self,
        wallet: &Wallet,
        key: &EncryptedWalletKeyVersion,
        config: &WalletCreationConfig,
    ) -> Result<Wallet>;

    /// Registers a read-only external recipient. No private-key row is
    /// created, so this wallet can never be used by the signer. Strict EVM
    /// addresses use the same canonical-address rule as local wallets even
    /// when the custody algorithm is `external`.
    async fn create_external(&self, wallet: &Wallet) -> Result<Wallet>;

    /// Creates an external recipient and its initial lifecycle/limit metadata
    /// atomically.
    async fn create_external_configured(&self, wallet: &Wallet, config: &WalletCreationConfig) -> Result<Wallet>;

    async fn get(&self, wallet_id: &str) -> Result<Option<Wallet>>;

    /// Returns lifecycle metadata used by payment authorization and price
    /// resolution. Implementations derive `active_key_version` from the one
    /// `wallet_key_version` row whose status is `active`; they must not infer
    /// signing readiness from `pay_wallet` metadata alone.
    async fn get_payment_wallet(&self, wallet_id: &str) -> Result<Option<PaymentWallet>>;

    async fn list(&self, page: PageRequest) -> Result<Page<Wallet>>;

    async fn load_active_key(&self, wallet_id: &str) -> Result<Option<EncryptedWalletKeyVersion>>;

    async fn list_key_versions(&self, wallet_id: &str) -> Result<Vec<EncryptedWalletKeyVersion>>;

    /// Retires the previous active version, inserts `key`, and updates public
    /// wallet metadata atomically. Historical versions cannot be deleted.
    async fn rotate(&self, wallet: &Wallet, key: &EncryptedWalletKeyVersion, retired_at: i64) -> Result<Wallet>;

    async fn mark_key_compromised(&self, wallet_id: &str, version: u32, retired_at: i64) -> Result<()>;

    /// Wallet-wide limits cap aggregate same-direction flow across all sharing
    /// scenarios, networks, and token contracts in one explicit denomination. Storage
    /// must reject a unit that differs from any policy already bound to the
    /// wallet and must not clear/change it while such policies exist.
    async fn set_spending_limits(
        &self,
        wallet_id: &str,
        budget_unit: &BudgetUnit,
        max_per_payment: Option<Amount>,
        max_per_day: Option<Amount>,
        updated_at: i64,
    ) -> Result<()>;

    async fn set_enabled(&self, wallet_id: &str, enabled: bool, updated_at: i64) -> Result<()>;
}

/// Persistence port for periodic wallet-balance observations.
#[async_trait]
pub trait IWalletBalanceRepository: Send + Sync {
    async fn upsert_balance(&self, balance: &WalletBalanceRecord) -> Result<WalletBalanceRecord>;

    async fn get_balance(&self, wallet_id: &str, asset: &AssetIdentity) -> Result<Option<WalletBalanceRecord>>;
}

#[cfg(test)]
#[path = "../../../../tests/storage/wallet/mod_test.rs"]
mod tests;

mod validation {
    use crate::{
        model::Wallet,
        storage::{EncryptedWalletKeyVersion, WalletCreationConfig, WalletKeyStatus},
        Error, Result,
    };

    pub(crate) struct WalletStorageRules;
    impl WalletStorageRules {
        pub(crate) fn canonical_wallet(wallet: &Wallet) -> Result<Wallet> {
            if wallet.id.trim().is_empty() || wallet.algorithm.trim().is_empty() || wallet.address.trim().is_empty() {
                return Err(Error::InvalidRequest("wallet identity is incomplete".to_owned()));
            }
            wallet
                .clone()
                .canonicalized()
                .map_err(|field| Error::InvalidRequest(format!("invalid wallet {field}")))
        }
        pub(crate) fn validate_creation_config(config: &WalletCreationConfig) -> Result<()> {
            config
                .validate()
                .map_err(|field| Error::InvalidRequest(format!("invalid initial wallet {field}")))
        }
        pub(crate) fn validate_key_for_wallet(
            wallet: &Wallet,
            key: &EncryptedWalletKeyVersion,
            require_active: bool,
        ) -> Result<()> {
            if key.wallet_id != wallet.id
                || key.algorithm != wallet.algorithm
                || key.address != wallet.address
                || key.public_key != wallet.public_key
                || key.version == 0
                || key.encryption_scheme.trim().is_empty()
                || key.kek_id.trim().is_empty()
                || key.wrapped_data_key.is_empty()
                || key.wrap_nonce.is_empty()
                || key.key_ciphertext.is_empty()
                || key.key_nonce.is_empty()
                || key.aad.is_empty()
                || (require_active && key.status != WalletKeyStatus::Active)
                || key.retired_at.is_some()
            {
                return Err(Error::InvalidKey(
                    "encrypted wallet key metadata is inconsistent".to_owned(),
                ));
            }
            Ok(())
        }
    }
}
pub(crate) use validation::*;

#[path = "macro/custody.rs"]
mod custody;
pub(crate) use custody::WalletKeyRepository;

pub use crate::model::wallet_balance::WalletBalanceRecord;
pub use crate::model::wallet_key::{EncryptedWalletKeyVersion, WalletKeyStatus};
