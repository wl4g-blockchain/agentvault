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
#[path = "../../../tests/storage/wallet/mod_test.rs"]
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

mod custody {
    // Shared versioned wallet custody; SQL repositories only persist its envelopes.
    use crate::{
        config::WalletKeyRingConfig, model::wallet as crypto, model::wallet::is_evm_wallet_algorithm,
        model::wallet::is_solana_wallet_algorithm, model::wallet::Wallet, model::wallet::EVM_EOA_ALGORITHM,
        storage::EncryptedWalletKeyVersion, storage::IWalletRepository, storage::IntentBoundPaymentSignatureRequest,
        storage::WalletCreationConfig, storage::WalletKeyManager, storage::WalletKeyStatus,
        storage::WalletPaymentSignature, storage::WalletPaymentSigner, util::aead::Aes256GcmUuidNonce,
        util::aead::UuidNonce, util::aead::UuidNonceCodec, util::aead::WALLET_KEY_ENCRYPTION_SCHEME,
        util::secret::SecretReader, Error, Result,
    };
    use aes_gcm::{aead::Aead as _, aead::KeyInit as _, aead::Payload};
    use async_trait::async_trait;
    use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
    use rand_core::{OsRng, RngCore as _};
    use std::{collections::BTreeMap, fmt};
    use zeroize::{Zeroize as _, Zeroizing};

    const KEY_BYTES: usize = 32;
    const MAX_ENCODED_KEK_BYTES: usize = 256;
    const AAD_DOMAIN: &[u8] = b"agentvault.wallet-key.v2";

    /// Versioned envelope encryption and SQL persistence for local EOA keys.
    ///
    /// KEKs are resolved only from environment-variable references at startup.
    /// The active key encrypts new versions; historical KEKs remain decrypt-only
    /// until no retained wallet version references them.
    pub struct WalletKeyRepository<S: IWalletRepository> {
        storage: S,
        active_kek_id: String,
        keks: BTreeMap<String, Zeroizing<[u8; KEY_BYTES]>>,
    }

    #[async_trait]
    impl<S: IWalletRepository> WalletPaymentSigner for WalletKeyRepository<S> {
        async fn sign_payment(
            &self,
            request: IntentBoundPaymentSignatureRequest<'_>,
        ) -> Result<WalletPaymentSignature> {
            let intent_wallet_id = request
                .intent()
                .wallet_id
                .as_deref()
                .ok_or_else(|| Error::InvalidRequest("payment intent has no wallet binding".to_owned()))?;
            if intent_wallet_id != request.wallet().wallet.id
                || request.active_key().wallet_id != request.wallet().wallet.id
                || request.wallet().active_key_version != Some(request.active_key().version)
                || request.active_key().status != WalletKeyStatus::Active
                || !request
                    .wallet()
                    .signing_ready_for_payment_network(&request.intent().asset.network)
                || request.intent().challenge_hash.as_deref() != Some(request.challenge().challenge_hash.as_str())
            {
                return Err(Error::PaymentConflict(
                    "intent-bound signer received inconsistent wallet authority".to_owned(),
                ));
            }
            let private_key = self.decrypt_version(request.active_key())?;
            let algorithm = request.wallet().wallet.algorithm.as_str();
            let bytes = if is_evm_wallet_algorithm(algorithm) {
                crypto::WalletCrypto::sign_eip712_digest(&private_key, request.digest())?.to_vec()
            } else if is_solana_wallet_algorithm(algorithm) {
                crypto::WalletCrypto::sign_solana_message(&private_key, request.digest())?.to_vec()
            } else {
                return Err(Error::InvalidKey("unsupported payment wallet algorithm".to_owned()));
            };
            Ok(WalletPaymentSignature {
                wallet_id: request.wallet().wallet.id.clone(),
                key_version: request.active_key().version,
                signer_address: request.wallet().wallet.address.clone(),
                bytes,
            })
        }
    }

    #[async_trait]
    impl<S: IWalletRepository> WalletKeyManager for WalletKeyRepository<S> {
        async fn create_local(
            &self,
            wallet_id: &str,
            algorithm: &str,
            private_key: &[u8],
            config: &WalletCreationConfig,
            created_at: i64,
        ) -> Result<Wallet> {
            self.create_local_configured_for_algorithm(wallet_id, algorithm, private_key, config, created_at)
                .await
        }

        async fn rotate(&self, wallet_id: &str, private_key: &[u8], activated_at: i64) -> Result<Wallet> {
            Self::rotate(self, wallet_id, private_key, activated_at).await
        }
    }

    impl<S: IWalletRepository> fmt::Debug for WalletKeyRepository<S> {
        fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter
                .debug_struct("WalletKeyRepository")
                .field("active_kek_id", &self.active_kek_id)
                .field("kek_ids", &self.keks.keys().collect::<Vec<_>>())
                .finish_non_exhaustive()
        }
    }

    impl<S: IWalletRepository> WalletKeyRepository<S> {
        /// Resolves a KEK ring from environment-only secret references.
        ///
        /// # Errors
        ///
        /// Fails when an inline/file source is selected, an environment variable
        /// is missing, a value is not unpadded URL-safe base64 for exactly 32
        /// bytes, or the active key id is unavailable.
        pub fn from_config(storage: impl Into<S>, config: &WalletKeyRingConfig) -> Result<Self> {
            if config.keys.is_empty() || !config.keys.contains_key(&config.active_key_id) {
                return Err(Error::Config("wallet KEK active version is unavailable".to_owned()));
            }
            let mut keks = BTreeMap::new();
            for (key_id, reference) in &config.keys {
                if key_id.trim().is_empty() || !reference.file.is_empty() || reference.env.trim().is_empty() {
                    return Err(Error::Config(
                        "wallet KEKs must use named environment references".to_owned(),
                    ));
                }
                let encoded = SecretReader::read_env_value(
                    reference.env.trim(),
                    &format!("wallet KEK {key_id}"),
                    MAX_ENCODED_KEK_BYTES,
                )?;
                if encoded.len() > MAX_ENCODED_KEK_BYTES {
                    return Err(Error::Config(format!("wallet KEK {key_id} is too large")));
                }
                let mut decoded = Zeroizing::new(
                    URL_SAFE_NO_PAD
                        .decode(encoded.as_bytes())
                        .map_err(|_| Error::Config(format!("wallet KEK {key_id} is not valid base64")))?,
                );
                if decoded.len() != KEY_BYTES {
                    return Err(Error::Config(format!("wallet KEK {key_id} must contain 32 bytes")));
                }
                let mut key = Zeroizing::new([0_u8; KEY_BYTES]);
                key.copy_from_slice(&decoded);
                decoded.zeroize();
                if keks.insert(key_id.clone(), key).is_some() {
                    return Err(Error::Config(format!("duplicate wallet KEK id {key_id}")));
                }
            }
            Ok(Self {
                storage: storage.into(),
                active_kek_id: config.active_key_id.clone(),
                keks,
            })
        }

        #[cfg(test)]
        pub(crate) fn from_resolved_keys(
            storage: impl Into<S>,
            active_kek_id: &str,
            keys: impl IntoIterator<Item = (String, [u8; KEY_BYTES])>,
        ) -> Result<Self> {
            let resolved_keys = keys
                .into_iter()
                .map(|(key_id, key)| (key_id, Zeroizing::new(key)))
                .collect::<BTreeMap<_, _>>();
            if active_kek_id.trim().is_empty() || resolved_keys.is_empty() || !resolved_keys.contains_key(active_kek_id)
            {
                return Err(Error::Config("wallet KEK active version is unavailable".to_owned()));
            }
            Ok(Self {
                storage: storage.into(),
                active_kek_id: active_kek_id.to_owned(),
                keks: resolved_keys,
            })
        }

        /// Creates a local EOA wallet and its encrypted initial key in one SQL
        /// transaction.
        ///
        /// # Errors
        ///
        /// Fails when the identifier or private key is invalid, encryption fails,
        /// or the wallet/key aggregate cannot be inserted atomically.
        pub async fn create_local(&self, wallet_id: &str, private_key: &[u8], created_at: i64) -> Result<Wallet> {
            self.create_local_configured(
                wallet_id,
                private_key,
                &WalletCreationConfig {
                    enabled: true,
                    ..WalletCreationConfig::default()
                },
                created_at,
            )
            .await
        }

        /// Creates a local EOA wallet, encrypted initial key, lifecycle state, and
        /// wallet-level limits in one SQL transaction.
        ///
        /// # Errors
        ///
        /// Fails when key material or initial metadata is invalid, encryption
        /// fails, or the complete wallet aggregate cannot be committed atomically.
        pub async fn create_local_configured(
            &self,
            wallet_id: &str,
            private_key: &[u8],
            config: &WalletCreationConfig,
            created_at: i64,
        ) -> Result<Wallet> {
            self.create_local_configured_for_algorithm(wallet_id, EVM_EOA_ALGORITHM, private_key, config, created_at)
                .await
        }

        /// Creates a local wallet for one explicitly supported custody
        /// algorithm while preserving the existing EVM convenience API.
        ///
        /// # Errors
        ///
        /// Fails when the algorithm or key material is invalid, encryption
        /// fails, or the complete wallet aggregate cannot be committed atomically.
        pub async fn create_local_configured_for_algorithm(
            &self,
            wallet_id: &str,
            algorithm: &str,
            private_key: &[u8],
            config: &WalletCreationConfig,
            created_at: i64,
        ) -> Result<Wallet> {
            Self::validate_wallet_id(wallet_id)?;
            let wallet = Self::wallet_from_private_key(wallet_id, algorithm, private_key, created_at)?;
            let encrypted = self.encrypt_version(&wallet, private_key, 1, created_at)?;
            self.storage.create_local_configured(&wallet, &encrypted, config).await
        }

        /// Rotates a local wallet to a new immutable encrypted key version.
        ///
        /// # Errors
        ///
        /// Fails when the wallet has no active local key, the replacement key is
        /// invalid, encryption fails, or the compare-and-swap rotation conflicts.
        pub async fn rotate(&self, wallet_id: &str, private_key: &[u8], activated_at: i64) -> Result<Wallet> {
            let current = self
                .storage
                .get_payment_wallet(wallet_id)
                .await?
                .ok_or_else(|| Error::NotFound(format!("payment wallet {wallet_id}")))?;
            let current_version = current
                .active_key_version
                .ok_or_else(|| Error::InvalidTransition("wallet has no active key to rotate".to_owned()))?;
            let next_version = current_version
                .checked_add(1)
                .ok_or_else(|| Error::InvalidTransition("wallet key version overflow".to_owned()))?;
            let wallet = Self::wallet_from_private_key(
                wallet_id,
                &current.wallet.algorithm,
                private_key,
                current.wallet.created_at,
            )?;
            let encrypted = self.encrypt_version(&wallet, private_key, next_version, activated_at)?;
            self.storage.rotate(&wallet, &encrypted, activated_at).await
        }

        /// Produces the immutable SQL envelope for one EOA version.
        ///
        /// # Errors
        ///
        /// Fails when metadata is invalid, the private key does not match the
        /// wallet, the active KEK is unavailable, or authenticated encryption fails.
        pub fn encrypt_version(
            &self,
            wallet: &Wallet,
            private_key: &[u8],
            version: u32,
            activated_at: i64,
        ) -> Result<EncryptedWalletKeyVersion> {
            if version == 0 || activated_at < 0 {
                return Err(Error::InvalidKey("wallet key version metadata is invalid".to_owned()));
            }
            Self::verify_private_key_matches_wallet(wallet, private_key)?;
            let aad = Self::key_aad(wallet, version, &self.active_kek_id);
            let kek = self
                .keks
                .get(&self.active_kek_id)
                .ok_or_else(|| Error::Crypto("active wallet KEK is unavailable".to_owned()))?;
            let mut data_key = Zeroizing::new([0_u8; KEY_BYTES]);
            let wrap_nonce = UuidNonceCodec::generate();
            let key_nonce = UuidNonceCodec::generate();
            OsRng.fill_bytes(data_key.as_mut());
            let wrap_aad = Self::wrapping_aad(&aad, &self.active_kek_id);
            let kek_cipher = Aes256GcmUuidNonce::new_from_slice(kek.as_ref())
                .map_err(|_| Error::Crypto("wallet KEK length is invalid".to_owned()))?;
            let wrapped_data_key = kek_cipher
                .encrypt(
                    UuidNonce::from_slice(&wrap_nonce),
                    Payload {
                        msg: data_key.as_ref(),
                        aad: &wrap_aad,
                    },
                )
                .map_err(|_| Error::Crypto("wallet data-key wrapping failed".to_owned()))?;
            let data_cipher = Aes256GcmUuidNonce::new_from_slice(data_key.as_ref())
                .map_err(|_| Error::Crypto("wallet data-key length is invalid".to_owned()))?;
            let key_ciphertext = data_cipher
                .encrypt(
                    UuidNonce::from_slice(&key_nonce),
                    Payload {
                        msg: private_key,
                        aad: &aad,
                    },
                )
                .map_err(|_| Error::Crypto("wallet private-key encryption failed".to_owned()))?;
            Ok(EncryptedWalletKeyVersion {
                wallet_id: wallet.id.clone(),
                version,
                algorithm: wallet.algorithm.clone(),
                address: wallet.address.clone(),
                public_key: wallet.public_key.clone(),
                encryption_scheme: WALLET_KEY_ENCRYPTION_SCHEME.to_owned(),
                kek_id: self.active_kek_id.clone(),
                wrapped_data_key,
                wrap_nonce: wrap_nonce.to_vec(),
                key_ciphertext,
                key_nonce: key_nonce.to_vec(),
                aad,
                status: WalletKeyStatus::Active,
                activated_at,
                retired_at: None,
                created_at: activated_at,
            })
        }

        fn decrypt_version(&self, key: &EncryptedWalletKeyVersion) -> Result<Zeroizing<Vec<u8>>> {
            if key.encryption_scheme != WALLET_KEY_ENCRYPTION_SCHEME || key.status != WalletKeyStatus::Active {
                return Err(Error::Crypto(
                    "wallet key envelope is not an active supported version".to_owned(),
                ));
            }
            let wallet = Wallet {
                id: key.wallet_id.clone(),
                algorithm: key.algorithm.clone(),
                address: key.address.clone(),
                public_key: key.public_key.clone(),
                created_at: 0,
            };
            let expected_aad = Self::key_aad(&wallet, key.version, &key.kek_id);
            if !Self::constant_time_eq(&expected_aad, &key.aad) {
                return Err(Error::Crypto(
                    "wallet key envelope metadata authentication failed".to_owned(),
                ));
            }
            let wrap_nonce = UuidNonceCodec::parse(&key.wrap_nonce)
                .map_err(|_| Error::Crypto("wallet key wrapping nonce is invalid".to_owned()))?;
            let key_nonce = UuidNonceCodec::parse(&key.key_nonce)
                .map_err(|_| Error::Crypto("wallet key nonce is invalid".to_owned()))?;
            let kek = self
                .keks
                .get(&key.kek_id)
                .ok_or_else(|| Error::Crypto("wallet KEK version is unavailable".to_owned()))?;
            let wrap_aad = Self::wrapping_aad(&key.aad, &key.kek_id);
            let kek_cipher = Aes256GcmUuidNonce::new_from_slice(kek.as_ref())
                .map_err(|_| Error::Crypto("wallet KEK length is invalid".to_owned()))?;
            let mut data_key = Zeroizing::new(
                kek_cipher
                    .decrypt(
                        UuidNonce::from_slice(&wrap_nonce),
                        Payload {
                            msg: &key.wrapped_data_key,
                            aad: &wrap_aad,
                        },
                    )
                    .map_err(|_| Error::Crypto("wallet data-key authentication failed".to_owned()))?,
            );
            if data_key.len() != KEY_BYTES {
                return Err(Error::Crypto("wallet data key has an invalid length".to_owned()));
            }
            let data_cipher = Aes256GcmUuidNonce::new_from_slice(data_key.as_slice())
                .map_err(|_| Error::Crypto("wallet data-key length is invalid".to_owned()))?;
            let plaintext = data_cipher
                .decrypt(
                    UuidNonce::from_slice(&key_nonce),
                    Payload {
                        msg: &key.key_ciphertext,
                        aad: &key.aad,
                    },
                )
                .map(Zeroizing::new)
                .map_err(|_| Error::Crypto("wallet private-key authentication failed".to_owned()))?;
            data_key.zeroize();
            Self::verify_private_key_matches_wallet(&wallet, &plaintext)?;
            Ok(plaintext)
        }

        fn wallet_from_private_key(
            wallet_id: &str,
            algorithm: &str,
            private_key: &[u8],
            created_at: i64,
        ) -> Result<Wallet> {
            crypto::WalletCrypto::validate_private_key_for_algorithm(algorithm, private_key)?;
            let (address, public_key) = crypto::WalletCrypto::payment_key_metadata(algorithm, private_key)?;
            Ok(Wallet {
                id: wallet_id.to_owned(),
                algorithm: algorithm.to_owned(),
                address,
                public_key,
                created_at,
            })
        }

        fn verify_private_key_matches_wallet(wallet: &Wallet, private_key: &[u8]) -> Result<()> {
            crypto::WalletCrypto::validate_private_key_for_algorithm(&wallet.algorithm, private_key)?;
            let (address, public_key) = crypto::WalletCrypto::payment_key_metadata(&wallet.algorithm, private_key)?;
            if address != wallet.address || public_key != wallet.public_key {
                return Err(Error::InvalidKey(
                    "private key does not match wallet public metadata".to_owned(),
                ));
            }
            Ok(())
        }

        fn validate_wallet_id(wallet_id: &str) -> Result<()> {
            if wallet_id.trim().is_empty() || wallet_id.len() > 256 || wallet_id.chars().any(char::is_control) {
                return Err(Error::InvalidRequest("wallet id is invalid".to_owned()));
            }
            Ok(())
        }

        fn key_aad(wallet: &Wallet, version: u32, kek_id: &str) -> Vec<u8> {
            let version = version.to_be_bytes();
            let fields = [
                wallet.id.as_bytes(),
                version.as_slice(),
                wallet.algorithm.as_bytes(),
                wallet.address.as_bytes(),
                wallet.public_key.as_bytes(),
                kek_id.as_bytes(),
            ];
            let mut aad = Vec::with_capacity(512);
            aad.extend_from_slice(AAD_DOMAIN);
            for field in fields {
                aad.extend_from_slice(&u64::try_from(field.len()).unwrap_or(u64::MAX).to_be_bytes());
                aad.extend_from_slice(field);
            }
            aad
        }

        fn wrapping_aad(aad: &[u8], kek_id: &str) -> Vec<u8> {
            let mut output = Vec::with_capacity(aad.len() + kek_id.len() + 16);
            output.extend_from_slice(b"agentvault.wrap.v1");
            output.extend_from_slice(aad);
            output.extend_from_slice(kek_id.as_bytes());
            output
        }

        fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
            if left.len() != right.len() {
                return false;
            }
            left.iter()
                .zip(right)
                .fold(0_u8, |difference, (left, right)| difference | (left ^ right))
                == 0
        }
    }

    #[cfg(test)]
    mod tests {
        include!(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/storage/wallet/key_test.rs"));
    }
}
pub(crate) use custody::WalletKeyRepository;

pub use crate::model::wallet_balance::WalletBalanceRecord;
pub use crate::model::wallet_key::{EncryptedWalletKeyVersion, WalletKeyStatus};
