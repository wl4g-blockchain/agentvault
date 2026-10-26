//! Wallet persistence contract.

use crate::{
    model::{Amount, AssetIdentity, BudgetUnit, Wallet},
    storage::{Page, PageRequest, WalletPaymentSigner},
    Result,
};
use async_trait::async_trait;
use std::fmt;

pub(crate) mod key;
pub mod postgres;
pub mod sqlite;
pub(crate) mod transactions;

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
pub enum WalletKeyStatus {
    Active,
    Retired,
    Compromised,
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

/// Last successful chain observation for one managed wallet and asset.
/// This record is operational inventory, never authorization input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletBalanceRecord {
    pub wallet_id: String,
    pub asset: AssetIdentity,
    pub asset_available: Amount,
    pub native_available: Amount,
    pub observed_at: i64,
    pub updated_at: i64,
}

impl WalletBalanceRecord {
    /// Validates the persisted observation identity and timestamps.
    ///
    /// # Errors
    ///
    /// Returns the invalid field name.
    pub fn validate(&self) -> std::result::Result<(), &'static str> {
        if self.wallet_id.trim().is_empty() {
            return Err("wallet_id");
        }
        self.asset.validate()?;
        if self.observed_at < 0 || self.updated_at < self.observed_at {
            return Err("observed_at");
        }
        Ok(())
    }
}

impl PaymentWallet {
    /// Validates and canonicalizes payment-facing wallet metadata loaded from
    /// storage before it participates in policy or offer evaluation.
    ///
    /// # Errors
    ///
    /// Returns `address` for malformed EVM wallet metadata.
    pub fn canonicalized(mut self) -> std::result::Result<Self, &'static str> {
        self.wallet = self.wallet.canonicalized()?;
        self.validate_payment_metadata()?;
        Ok(self)
    }

    /// Also validates the recipient address family required by the offer's
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

impl WalletKeyStatus {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Retired => "retired",
            Self::Compromised => "compromised",
        }
    }
}

/// Envelope-encrypted private-key material for one immutable wallet version.
/// Plaintext private keys are intentionally not representable by this port.
#[derive(Clone, PartialEq, Eq)]
pub struct EncryptedWalletKeyVersion {
    pub wallet_id: String,
    pub version: u32,
    pub algorithm: String,
    pub address: String,
    pub public_key: String,
    pub encryption_scheme: String,
    pub kek_id: String,
    pub wrapped_data_key: Vec<u8>,
    pub wrap_nonce: Vec<u8>,
    pub key_ciphertext: Vec<u8>,
    pub key_nonce: Vec<u8>,
    pub aad: Vec<u8>,
    pub status: WalletKeyStatus,
    pub activated_at: i64,
    pub retired_at: Option<i64>,
    pub created_at: i64,
}

impl fmt::Debug for EncryptedWalletKeyVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EncryptedWalletKeyVersion")
            .field("wallet_id", &self.wallet_id)
            .field("version", &self.version)
            .field("algorithm", &self.algorithm)
            .field("address", &self.address)
            .field("public_key", &self.public_key)
            .field("encryption_scheme", &self.encryption_scheme)
            .field("kek_id", &self.kek_id)
            .field("wrapped_data_key", &"<redacted>")
            .field("wrap_nonce", &"<redacted>")
            .field("key_ciphertext", &"<redacted>")
            .field("key_nonce", &"<redacted>")
            .field("aad", &"<redacted>")
            .field("status", &self.status)
            .field("activated_at", &self.activated_at)
            .field("retired_at", &self.retired_at)
            .field("created_at", &self.created_at)
            .finish()
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

    /// Returns lifecycle metadata used by payment authorization and offer
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
pub trait WalletBalanceStorage: Send + Sync {
    async fn upsert_balance(&self, balance: &WalletBalanceRecord) -> Result<WalletBalanceRecord>;

    async fn get_balance(&self, wallet_id: &str, asset: &AssetIdentity) -> Result<Option<WalletBalanceRecord>>;
}

#[cfg(test)]
#[path = "../../../tests/storage/wallet/mod_test.rs"]
mod tests;

/// Compatibility import for the single repository contract.
pub use IWalletRepository as WalletStorage;

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
