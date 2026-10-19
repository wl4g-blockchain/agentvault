//! Shared durable-storage contracts, provider lifecycle, and aggregate transaction ports.

use std::{collections::BTreeMap, future::Future, pin::Pin, sync::Arc};

use async_trait::async_trait;
use tokio::sync::Mutex;

use crate::{
    config::{StorageConfig, StorageProvider, WalletKeyRingConfig},
    model::wallet::Wallet,
    utils::secret::read_secret_ref,
    Error, Result,
};

use super::{postgres::PostgresStorage, sqlite::SqliteStorage};

const MAX_STORAGE_DSN_BYTES: usize = 16 * 1024;

mod catalog {
    mod approver {
        use async_trait::async_trait;

        use crate::{model::PaymentScenarioApprover, Result};

        use super::{Page, PageRequest};

        /// Durable scenario-administrator mappings. Principals originate from trusted
        /// authentication and are scoped to exactly one scenario.
        #[async_trait]
        pub trait ApproverStorage: Send + Sync {
            async fn upsert(&self, approver: &PaymentScenarioApprover) -> Result<PaymentScenarioApprover>;

            async fn get(&self, scenario_id: &str, auth_principal: &str) -> Result<Option<PaymentScenarioApprover>>;

            /// Returns true only when the mapping, scenario, and owning client are all
            /// enabled at the same database snapshot.
            async fn is_authorized(&self, scenario_id: &str, auth_principal: &str) -> Result<bool>;

            async fn set_enabled(
                &self,
                scenario_id: &str,
                auth_principal: &str,
                enabled: bool,
                updated_at: i64,
            ) -> Result<PaymentScenarioApprover>;

            async fn list_for_scenario(
                &self,
                scenario_id: &str,
                page: PageRequest,
            ) -> Result<Page<PaymentScenarioApprover>>;
        }
    }

    mod client {
        use async_trait::async_trait;

        use crate::{model::PaymentClient, Result};

        use super::{Page, PageRequest};

        #[async_trait]
        pub trait ClientStorage: Send + Sync {
            async fn insert(&self, client: &PaymentClient) -> Result<PaymentClient>;

            async fn get(&self, client_id: &str) -> Result<Option<PaymentClient>>;

            async fn find_by_principal(&self, auth_principal: &str) -> Result<Option<PaymentClient>>;

            async fn set_enabled(&self, client_id: &str, enabled: bool, updated_at: i64) -> Result<PaymentClient>;

            async fn list(&self, page: PageRequest) -> Result<Page<PaymentClient>>;
        }
    }

    mod offer {
        use async_trait::async_trait;

        use crate::{model::PaymentOffer, Result};

        use super::{Page, PageRequest};

        #[async_trait]
        pub trait OfferStorage: Send + Sync {
            /// Inserts an offer only when `(policy_id, wallet_id)` is an explicit
            /// `pay_policy_wallet` membership and the policy authorizes inbound flow
            /// for the offer's scenario and budget unit.
            async fn insert(&self, offer: &PaymentOffer) -> Result<PaymentOffer>;

            async fn get(&self, offer_id: &str) -> Result<Option<PaymentOffer>>;

            /// Resolves only the enabled offer owned by the authenticated scenario.
            /// Storage must enforce at most one active offer per `(scenario, resource)`;
            /// the row's protocol list represents alternate adapters for the same
            /// fixed price and recipient. Ambiguous rows are an invariant error, never
            /// a `LIMIT 1` selection.
            async fn resolve(&self, scenario_id: &str, resource: &str, protocol: &str) -> Result<Option<PaymentOffer>>;

            /// Resolves the single enabled offer for a scenario resource before
            /// the caller has selected one of the offer's supported protocols.
            /// This must be a direct authority query; callers must not infer the
            /// result from an arbitrary page of catalog rows.
            async fn resolve_resource(&self, scenario_id: &str, resource: &str) -> Result<Option<PaymentOffer>>;

            /// Replaces pricing or lifecycle fields with optimistic concurrency.
            async fn update(&self, offer: &PaymentOffer, expected_updated_at: i64) -> Result<PaymentOffer>;

            async fn list_for_scenario(&self, scenario_id: &str, page: PageRequest) -> Result<Page<PaymentOffer>>;
        }
    }

    mod policy {
        use async_trait::async_trait;

        use crate::{model::PaymentPolicy, Result};

        use super::{Page, PageRequest};

        #[async_trait]
        pub trait PolicyStorage: Send + Sync {
            /// Inserts the policy and every `pay_policy_wallet` membership atomically
            /// while locking all wallet rows in stable identifier order. Wallets
            /// without a budget unit are pinned to the policy unit; an existing
            /// different unit is rejected. Enabling a policy that makes any wallet
            /// shared across scenarios requires both wallet-wide hard limits in the
            /// same locked transaction.
            async fn insert(&self, policy: &PaymentPolicy) -> Result<PaymentPolicy>;

            async fn get(&self, policy_id: &str) -> Result<Option<PaymentPolicy>>;

            /// Replaces mutable policy fields using `expected_updated_at` as an
            /// optimistic concurrency token while locking/revalidating every bound
            /// wallet. Wallet membership, scenario, and budget unit are immutable;
            /// disabling a policy never clears wallet units.
            async fn update(&self, policy: &PaymentPolicy, expected_updated_at: i64) -> Result<PaymentPolicy>;

            async fn list_for_scenario(&self, scenario_id: &str, page: PageRequest) -> Result<Page<PaymentPolicy>>;

            async fn list_for_wallet(&self, wallet_id: &str, page: PageRequest) -> Result<Page<PaymentPolicy>>;

            /// Returns whether an enabled policy currently binds the wallet to
            /// the scenario. This authority check must not depend on pagination.
            async fn has_enabled_wallet_binding(&self, scenario_id: &str, wallet_id: &str) -> Result<bool>;
        }
    }

    mod scenario {
        use async_trait::async_trait;
        use serde::Serialize;

        use crate::{
            model::{PaymentPolicy, PaymentScenario},
            Result,
        };

        use super::{IdempotentWrite, Page, PageRequest};

        /// Atomically created Scenario configuration. Wallet membership belongs
        /// to the initial Policy aggregate rather than being duplicated on the
        /// Scenario row.
        #[derive(Clone, Debug, PartialEq, Eq, Serialize)]
        pub struct ScenarioConfiguration {
            pub scenario: PaymentScenario,
            pub policy: PaymentPolicy,
        }

        #[async_trait]
        pub trait ScenarioStorage: Send + Sync {
            /// Registers a scenario idempotently on its stable `id`.
            /// Different content under an existing key must return a conflict.
            async fn register(&self, scenario: &PaymentScenario) -> Result<IdempotentWrite<PaymentScenario>>;

            /// Registers a Scenario and its initial multi-wallet Policy in one
            /// database transaction. Replays must match both aggregates exactly;
            /// partial configuration is never exposed to payment authorization.
            async fn register_with_policy(
                &self,
                scenario: &PaymentScenario,
                policy: &PaymentPolicy,
            ) -> Result<IdempotentWrite<ScenarioConfiguration>>;

            async fn get(&self, scenario_id: &str) -> Result<Option<PaymentScenario>>;

            /// Ownership-aware lookup used after authenticating the calling client.
            async fn get_for_client(&self, client_id: &str, scenario_id: &str) -> Result<Option<PaymentScenario>>;

            async fn set_enabled(&self, scenario_id: &str, enabled: bool, updated_at: i64) -> Result<PaymentScenario>;

            async fn list_for_client(&self, client_id: &str, page: PageRequest) -> Result<Page<PaymentScenario>>;
        }
    }

    mod wallet {
        use std::fmt;

        use async_trait::async_trait;

        use crate::{
            model::{wallet::Wallet, Amount, AssetIdentity, BudgetUnit},
            Result,
        };

        use super::{Page, PageRequest};

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
        /// Keeping this in the wallet creation transaction prevents provisioning
        /// crashes from leaving an unprovable half-configured authority record.
        #[derive(Clone, Debug, Default, PartialEq, Eq)]
        pub struct WalletProvisioningConfig {
            pub enabled: bool,
            pub budget_unit: Option<BudgetUnit>,
            pub max_per_payment: Option<Amount>,
            pub max_per_day: Option<Amount>,
        }

        impl WalletProvisioningConfig {
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
            pub fn canonicalized_for_payment_network(
                mut self,
                network: &str,
            ) -> std::result::Result<Self, &'static str> {
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
                self.signing_ready()
                    && (!crate::model::wallet::is_evm_payment_network(network)
                        || crate::model::wallet::is_evm_wallet_algorithm(&self.wallet.algorithm))
                    && self.clone().canonicalized_for_payment_network(network).is_ok()
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
        pub trait WalletStorage: Send + Sync {
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
                config: &WalletProvisioningConfig,
            ) -> Result<Wallet>;

            /// Registers a read-only external recipient. No private-key row is
            /// created, so this wallet can never be used by the signer. Strict EVM
            /// addresses use the same canonical-address rule as local wallets even
            /// when the custody algorithm is `external`.
            async fn create_external(&self, wallet: &Wallet) -> Result<Wallet>;

            /// Creates an external recipient and its initial lifecycle/limit metadata
            /// atomically.
            async fn create_external_configured(
                &self,
                wallet: &Wallet,
                config: &WalletProvisioningConfig,
            ) -> Result<Wallet>;

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
            async fn rotate(&self, wallet: &Wallet, key: &EncryptedWalletKeyVersion, retired_at: i64)
                -> Result<Wallet>;

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
    }

    // Lifecycle repositories for Client, Scenario, Policy, Offer, Approver,
    // and Wallet aggregates. Financial state transitions live in `transaction`.

    use super::{IdempotentWrite, Page, PageRequest};

    pub use approver::ApproverStorage;
    pub use client::ClientStorage;
    pub use offer::OfferStorage;
    pub use policy::PolicyStorage;
    pub use scenario::{ScenarioConfiguration, ScenarioStorage};
    pub use wallet::{
        EncryptedWalletKeyVersion, PaymentWallet, WalletBalanceRecord, WalletBalanceStorage, WalletCustody,
        WalletKeyStatus, WalletProvisioningConfig, WalletStorage,
    };
}

mod signing {
    use async_trait::async_trait;

    use crate::{
        model::{PaymentChallenge, PaymentClient, PaymentEvent, PaymentIntent, PaymentPolicy, PaymentScenario},
        protocol::ProtocolSigningRequest,
        utils::secure_material::SealedSensitiveMaterial,
        Result,
    };

    use super::{EncryptedWalletKeyVersion, IdempotentWrite, OutboxMessage, PaymentWallet};

    /// Principal-owned identity common to both a new signature and a durable
    /// signed-payment replay.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct OutboundSigningIdentity {
        pub client: PaymentClient,
        pub scenario: PaymentScenario,
        pub intent: PaymentIntent,
        /// Complete public protocol challenge recovered from the immutable
        /// `ChallengeReceived` event. A hash alone is not sufficient to recreate
        /// an adapter-owned signing payload after a crash.
        pub challenge: PaymentChallenge,
    }

    /// Current authorization facts needed immediately before using a local key.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct OutboundSigningAuthorization {
        pub identity: OutboundSigningIdentity,
        pub policy: PaymentPolicy,
        pub wallet: PaymentWallet,
        pub active_key: EncryptedWalletKeyVersion,
    }

    /// AEAD-protected result of a completed outbound signature.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct PersistedSignedPayment {
        pub identity: OutboundSigningIdentity,
        pub credential_hash: String,
        pub sealed_credential: SealedSensitiveMaterial,
    }

    /// Authoritative read result. A signed replay deliberately does not require
    /// the old key to remain active: it returns the credential committed by the
    /// original transaction and can never cause another signature.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub enum OutboundSigningRead {
        Authorized(Box<OutboundSigningAuthorization>),
        Signed(Box<PersistedSignedPayment>),
    }

    /// Snapshots revalidated while the signing transaction holds all authority
    /// rows. These values are optimistic-concurrency tokens, never authorization
    /// assertions supplied by a caller.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct SigningGuard {
        pub principal: String,
        pub client_id: String,
        pub expected_client_updated_at: i64,
        pub scenario_id: String,
        pub expected_scenario_updated_at: i64,
        pub policy_id: String,
        pub expected_policy_updated_at: i64,
        pub wallet_id: String,
        pub expected_wallet_updated_at: i64,
        pub expected_key_version: u32,
        pub expected_intent_updated_at: i64,
        pub challenge_hash: String,
        pub evaluated_at: i64,
    }

    /// One atomic `Authorized -> Signed` persistence operation.
    pub struct SigningWrite<'a> {
        pub intent: &'a PaymentIntent,
        pub challenge: &'a PaymentChallenge,
        pub credential_hash: &'a str,
        pub sealed_credential: &'a SealedSensitiveMaterial,
        pub guard: &'a SigningGuard,
        pub events: &'a [PaymentEvent],
        pub outbox: &'a [OutboxMessage],
    }

    /// Intent-bound request accepted by a wallet payment signer.
    ///
    /// The constructor is crate-private so HTTP, MQTT, and SDK transports cannot
    /// turn this port into an arbitrary-digest signing oracle. Only the payment
    /// application service can combine an authorized intent with an
    /// adapter-produced [`ProtocolSigningRequest`].
    pub struct IntentBoundPaymentSignatureRequest<'a> {
        intent: &'a PaymentIntent,
        challenge: &'a PaymentChallenge,
        wallet: &'a PaymentWallet,
        active_key: &'a EncryptedWalletKeyVersion,
        protocol_request: &'a ProtocolSigningRequest,
    }

    impl<'a> IntentBoundPaymentSignatureRequest<'a> {
        pub(crate) const fn new(
            intent: &'a PaymentIntent,
            challenge: &'a PaymentChallenge,
            wallet: &'a PaymentWallet,
            active_key: &'a EncryptedWalletKeyVersion,
            protocol_request: &'a ProtocolSigningRequest,
        ) -> Self {
            Self {
                intent,
                challenge,
                wallet,
                active_key,
                protocol_request,
            }
        }

        #[must_use]
        pub const fn intent(&self) -> &PaymentIntent {
            self.intent
        }

        #[must_use]
        pub const fn challenge(&self) -> &PaymentChallenge {
            self.challenge
        }

        #[must_use]
        pub const fn wallet(&self) -> &PaymentWallet {
            self.wallet
        }

        #[must_use]
        pub const fn active_key(&self) -> &EncryptedWalletKeyVersion {
            self.active_key
        }

        /// Adapter-produced digest. No public constructor accepts raw bytes.
        #[must_use]
        pub fn digest(&self) -> &[u8] {
            &self.protocol_request.digest
        }
    }

    /// Recoverable signature together with the key identity reported by the
    /// custody adapter. The handler rejects a result for any other wallet/version.
    #[derive(Clone, Debug, PartialEq, Eq)]
    pub struct WalletPaymentSignature {
        pub wallet_id: String,
        pub key_version: u32,
        pub signer_address: String,
        pub bytes: Vec<u8>,
    }

    /// Local-custody boundary used only for an already authorized payment.
    /// Implementations unwrap the supplied encrypted key version inside their
    /// custody boundary and must not expose a separate raw-digest transport API.
    #[async_trait]
    pub trait WalletPaymentSigner: Send + Sync {
        async fn sign_payment(&self, request: IntentBoundPaymentSignatureRequest<'_>)
            -> Result<WalletPaymentSignature>;
    }

    /// Atomic persistence boundary for outbound signing.
    #[async_trait]
    pub trait SigningStorage: Send + Sync {
        /// Loads an intent through `expected_principal -> enabled client -> owned
        /// scenario`. `None` is reserved for the explicitly configured open HTTP
        /// edge; the handler replaces it with the persisted client principal
        /// before any signing commit. `Authorized` reads recover the complete challenge and join
        /// the exact enabled outbound policy, explicit policy-wallet membership,
        /// enabled local-custody wallet, and active non-compromised key. `Signed`
        /// reads recover the immutable challenge plus the previously sealed
        /// credential and never require or expose private-key material.
        async fn load_for_principal(
            &self,
            expected_principal: Option<&str>,
            intent_id: &str,
        ) -> Result<Option<OutboundSigningRead>>;

        /// Atomically performs `Authorized -> Signed`, appends exactly one
        /// `SignRequested` and one `Signed` event, and inserts the outgoing
        /// notification. While holding the relevant rows, implementations must:
        ///
        /// - re-resolve the principal, enabled client, enabled owned scenario,
        ///   intent, enabled outbound policy, explicit policy-wallet membership,
        ///   enabled local wallet, and active non-compromised key;
        /// - require every revision/key version in [`SigningGuard`] to match;
        /// - require an unexpired outbound `Authorized` intent with a full
        ///   reservation equal to its amount and an immutable identity matching
        ///   `challenge` (including the complete event payload);
        /// - recheck protocol, network, asset, recipient, origin, and directional
        ///   policy scope before applying the compare-and-swap; and
        /// - store only the AEAD envelope, never credential or key plaintext.
        ///
        /// If a concurrent transaction already committed the same immutable
        /// intent and complete challenge, return `Replay` with the winning
        /// persisted envelope and discard the losing candidate. Adapter time
        /// fields may make two locally valid candidate credentials differ, but
        /// they must never replace the first committed credential. A differing
        /// challenge, authority guard, or signed identity is a conflict and must
        /// fail closed.
        async fn commit_signed(&self, write: SigningWrite<'_>) -> Result<IdempotentWrite<PersistedSignedPayment>>;
    }
}

mod transaction {
    mod approval {
        use async_trait::async_trait;

        use crate::{
            model::{
                ApprovalDecision, ApprovalGrant, ApprovalRequest, PaymentEvent, PaymentIntent, VerifiedApprovalGrant,
            },
            Result,
        };

        use super::{IdempotentWrite, OutboxMessage, Page, PageRequest};

        /// Atomic approval decision. Only JWT metadata and its AEAD envelope cross
        /// this boundary; the plaintext compact token is never persisted or returned.
        pub struct ApprovalWrite<'a> {
            pub decision: &'a ApprovalDecision,
            pub grant: Option<&'a ApprovalGrant>,
            pub intent_transition: Option<ApprovalIntentTransition<'a>>,
            pub event: &'a PaymentEvent,
            pub outbox: &'a [OutboxMessage],
        }

        /// Compare-and-swap transition applied with a rejection decision.
        #[derive(Clone, Copy, Debug)]
        pub struct ApprovalIntentTransition<'a> {
            pub intent: &'a PaymentIntent,
            pub expected_updated_at: i64,
        }

        /// Atomic one-time grant consumption and budget reservation.
        pub struct ApprovalConsumption<'a> {
            pub grant: &'a VerifiedApprovalGrant,
            pub intent: &'a PaymentIntent,
            pub guard: ApprovalReservationGuard<'a>,
            pub expected_intent_updated_at: i64,
            pub consumed_at: i64,
            pub event: &'a PaymentEvent,
            pub outbox: &'a [OutboxMessage],
        }

        /// Current request facts used to re-evaluate an approved payment inside the
        /// authoritative reservation transaction.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub struct ApprovalReservationGuard<'a> {
            pub origin: &'a str,
            pub day_started_at: i64,
            pub evaluated_at: i64,
        }

        #[async_trait]
        pub trait ApprovalStorage: Send + Sync {
            async fn request(&self, approval: &ApprovalRequest) -> Result<IdempotentWrite<ApprovalRequest>>;

            async fn get(&self, approval_id: &str) -> Result<Option<ApprovalRequest>>;

            async fn get_active_for_intent(&self, intent_id: &str) -> Result<Option<ApprovalRequest>>;

            async fn list_pending(
                &self,
                scenario_id: &str,
                now: i64,
                page: PageRequest,
            ) -> Result<Page<ApprovalRequest>>;

            /// Applies an authenticated administrator decision, records its event, and
            /// persists the JWT JTI, claims hash, sealed token, and notification in one transaction. The
            /// locked approval `intent_hash` must equal the locked intent
            /// `request_hash`; any mismatch is a conflict. Rejection
            /// must CAS the bound intent to `Failed` in that transaction. For an
            /// approval, the row's `expires_at` must become the grant's shorter expiry;
            /// the original pending-request deadline must never extend the configured
            /// grant lifetime.
            async fn decide(&self, write: ApprovalWrite<'_>) -> Result<ApprovalRequest>;

            /// Atomically consumes a matching, authenticated, unexpired JWT exactly once and
            /// reserves policy and shared-wallet budgets for the bound intent. The
            /// transaction must lock/reload the approval, intent, client, scenario,
            /// policy, wallet, and active key row; require
            /// `approval.intent_hash == intent.request_hash == grant.claims.intent_hash`,
            /// require the stored JTI and claims hash to match the verified grant,
            /// recheck enabled state, ownership,
            /// local custody, an active non-compromised signing key, complete policy
            /// scope, explicit policy-wallet membership, outbound direction, intent
            /// expiry, matching policy/wallet budget unit, and current hard limits.
            /// The approval may exceed the policy's automatic `max_per_day` only up to
            /// `max_per_day + approval_overage`; it can never bypass that ceiling or a
            /// wallet hard limit. If enabled policies from multiple scenarios share the
            /// wallet, both wallet-wide limits are mandatory. Both aggregates include
            /// every matching outbound `reserved` intent regardless of creation day,
            /// plus current-day confirmed outbound ledger facts selected by
            /// `effective_at`. Intent `created_at`, network, and token contract must
            /// not partition wallet usage. Only then may the
            /// grant be consumed and funds reserved. A lifecycle/policy change since
            /// approval fails closed.
            async fn consume_and_reserve(&self, consumption: ApprovalConsumption<'_>) -> Result<PaymentIntent>;

            /// Atomically expires due pending requests and approved, unconsumed grants.
            /// Each row's bound intent must transition to `Expired` with no reservation,
            /// and its expiry event/outbox notification must commit in the same
            /// transaction. Implementations use conditional updates so concurrent
            /// consumption or decisions win at most once.
            async fn expire_due(&self, now: i64, limit: u32) -> Result<Vec<ApprovalRequest>>;
        }
    }

    mod event {
        use async_trait::async_trait;

        use crate::{
            model::{PaymentEvent, PaymentEventType},
            Result,
        };

        use super::{IdempotentWrite, Page, PageRequest};

        /// Append-only operational history. Events are never used as the authoritative
        /// source for wallet totals.
        #[async_trait]
        pub trait EventStorage: Send + Sync {
            async fn append(&self, event: &PaymentEvent) -> Result<IdempotentWrite<PaymentEvent>>;

            async fn get(&self, event_id: &str) -> Result<Option<PaymentEvent>>;

            async fn find_for_intent_type(
                &self,
                intent_id: &str,
                event_type: PaymentEventType,
                attempt_no: u32,
            ) -> Result<Option<PaymentEvent>>;

            async fn list_for_intent(&self, intent_id: &str, page: PageRequest) -> Result<Page<PaymentEvent>>;
        }
    }

    mod intent {
        use async_trait::async_trait;

        use crate::{
            model::{
                Amount, ApprovalRequest, BudgetUnit, PaymentChallenge, PaymentClient, PaymentCredential,
                PaymentDirection, PaymentEvent, PaymentIntent, PaymentLedgerEntry, PaymentOffer, PaymentPolicy,
                PaymentScenario, PaymentStatus,
            },
            Result,
        };

        use super::{IdempotentWrite, OutboxMessage, Page, PageRequest, PaymentWallet};

        #[derive(Clone, Debug, Default, PartialEq, Eq)]
        pub struct PaymentQuery {
            pub scenario_id: Option<String>,
            pub wallet_id: Option<String>,
            pub run_id: Option<String>,
            pub direction: Option<PaymentDirection>,
            pub status: Option<PaymentStatus>,
            pub from_created_at: Option<i64>,
            pub to_created_at: Option<i64>,
            pub page: PageRequest,
        }

        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct ReconciliationLeaseRequest {
            pub owner: String,
            pub now: i64,
            pub lease_until: i64,
            pub limit: u32,
        }

        /// Atomic settlement/reconciliation commit. The intent transition, operation
        /// events, confirmed financial facts, and outgoing notifications either all
        /// commit or all roll back. For an outbound reserved intent this is the only
        /// valid path to `consumed`: the transaction must append its trusted ledger
        /// facts and consume the reservation together.
        pub struct SettlementWrite<'a> {
            pub intent: &'a PaymentIntent,
            pub expected_updated_at: i64,
            pub events: &'a [PaymentEvent],
            pub ledger_entries: &'a [PaymentLedgerEntry],
            pub outbox: &'a [OutboxMessage],
        }

        /// One durable non-financial lifecycle transition. This path records provider
        /// verification, retry, failure, or expiry while preserving an outstanding
        /// reservation (or releasing it on a terminal failure). It can never produce
        /// a signature, consume a reservation, mark an intent settled, or post money.
        pub struct PaymentProgressWrite<'a> {
            pub intent: &'a PaymentIntent,
            pub expected_updated_at: i64,
            pub events: &'a [PaymentEvent],
            pub outbox: &'a [OutboxMessage],
        }

        /// One untrusted merchant settlement hint for an outbound x402 intent.
        ///
        /// The transaction may only move `Signed`/`Unknown` to
        /// `PendingSettlement`, preserve the complete reservation and every financial
        /// field, and append one `Submitted` event whose transaction locator remains
        /// payload-only. In particular this boundary cannot populate the intent's
        /// trusted `tx_hash`, consume budget, or post a ledger entry.
        pub struct OutboundEvidenceWrite<'a> {
            pub intent: &'a PaymentIntent,
            pub expected_updated_at: i64,
            pub evidence_hash: &'a str,
            pub event: &'a PaymentEvent,
            pub outbox: &'a [OutboxMessage],
        }

        /// One payee challenge issuance transaction. The complete challenge is also
        /// carried by an immutable `ChallengeCreated` event so verification and crash
        /// recovery do not depend on process memory.
        pub struct ChallengeWrite<'a> {
            pub intent: &'a PaymentIntent,
            pub challenge: &'a PaymentChallenge,
            pub guard: &'a ChallengeIssuanceGuard,
            pub events: &'a [PaymentEvent],
            pub outbox: &'a [OutboxMessage],
        }

        /// Trusted snapshots that must still match while an inbound challenge is
        /// inserted. This closes the lifecycle/price/recipient race between preflight
        /// reads and durable challenge issuance.
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct ChallengeIssuanceGuard {
            pub principal: String,
            pub client: PaymentClient,
            pub scenario: PaymentScenario,
            pub offer: PaymentOffer,
            pub policy: PaymentPolicy,
            pub wallet: PaymentWallet,
            pub origin: String,
            pub day_started_at: i64,
            pub evaluated_at: i64,
        }

        /// One inbound credential verification transition. Implementations bind the
        /// credential fingerprint, update the intent, append events, and enqueue
        /// notifications in one database transaction. The unique protocol/credential
        /// index prevents a credential being redeemed by another request.
        pub struct VerificationWrite<'a> {
            pub intent: &'a PaymentIntent,
            pub credential: &'a PaymentCredential,
            pub expected_updated_at: i64,
            pub events: &'a [PaymentEvent],
            pub outbox: &'a [OutboxMessage],
        }

        /// One authorization transaction. With an approval row, the intent is stored
        /// without a reservation. Without one, policy and shared-wallet budgets are
        /// reserved before the intent becomes authorized. Events and notifications
        /// commit with the aggregate in both cases.
        pub struct AuthorizationWrite<'a> {
            pub intent: &'a PaymentIntent,
            pub approval: Option<&'a ApprovalRequest>,
            pub action: AuthorizationAction,
            pub guard: &'a AuthorizationGuard,
            pub events: &'a [PaymentEvent],
            pub outbox: &'a [OutboxMessage],
        }

        /// Snapshots and normalized request data that a storage adapter must validate
        /// again while holding the rows used by an authorization transaction.
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct AuthorizationGuard {
            pub principal: String,
            pub client: PaymentClient,
            pub scenario: PaymentScenario,
            pub policy: PaymentPolicy,
            pub expected_wallet_updated_at: i64,
            pub expected_wallet_key_version: Option<u32>,
            pub origin: String,
            pub day_started_at: i64,
            pub evaluated_at: i64,
        }

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum AuthorizationAction {
            RecordDenied,
            RequestApproval,
            Reserve,
        }

        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct AuthorizationRecord {
            pub intent: PaymentIntent,
            pub approval: Option<ApprovalRequest>,
        }

        /// Authoritative pending reservations plus same-day confirmed flow and
        /// wallet-wide hard limits. Pending usage includes every `reserved` intent in
        /// the requested direction regardless of creation day. Confirmed usage comes
        /// only from same-direction ledger facts
        /// whose `effective_at` is in the current UTC day. Both paths aggregate by
        /// scenario (across every current or historical policy) or wallet plus
        /// direction and budget asset+decimals,
        /// never network or token contract.
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct BudgetSnapshot {
            pub budget_unit: BudgetUnit,
            pub wallet_enabled: bool,
            pub wallet_algorithm: String,
            pub wallet_address: String,
            pub wallet_updated_at: i64,
            pub wallet_active_key_version: Option<u32>,
            /// Whether enabled policies from more than one scenario share the wallet.
            pub wallet_shared_across_scenarios: bool,
            pub scenario_spent_and_reserved: Amount,
            pub wallet_spent_and_reserved: Amount,
            pub wallet_max_per_payment: Option<Amount>,
            pub wallet_max_per_day: Option<Amount>,
        }

        impl BudgetSnapshot {
            #[must_use]
            pub const fn empty(budget_unit: BudgetUnit) -> Self {
                Self {
                    budget_unit,
                    wallet_enabled: false,
                    wallet_algorithm: String::new(),
                    wallet_address: String::new(),
                    wallet_updated_at: 0,
                    wallet_active_key_version: None,
                    wallet_shared_across_scenarios: false,
                    scenario_spent_and_reserved: Amount::ZERO,
                    wallet_spent_and_reserved: Amount::ZERO,
                    wallet_max_per_payment: None,
                    wallet_max_per_day: None,
                }
            }
        }

        #[async_trait]
        pub trait PaymentStorage: Send + Sync {
            /// Looks up the authoritative replay identity before re-evaluating mutable
            /// policy. Callers compare `request_hash` and reject mismatches.
            async fn find_run(&self, scenario_id: &str, run_id: &str) -> Result<Option<AuthorizationRecord>>;

            /// Atomically inserts an authorization aggregate, reserves both policy and
            /// wallet budgets when no approval is needed, and appends its events and
            /// outbox messages. The same transaction must lock/reload the client,
            /// scenario, policy, wallet, and active key row; verify their enabled
            /// state, ownership, local custody, active non-compromised signing key,
            /// snapshot revisions, request scope, challenge expiry, matching
            /// intent/policy/wallet budget unit, and current hard limits. A wallet
            /// shared by enabled policies from multiple scenarios must have both
            /// wallet-wide limits configured; missing limits fail closed. The
            /// transaction then
            /// recompute both budgets as all outstanding same-direction `reserved`
            /// intents in the unit (including prior days) plus current-day confirmed
            /// same-direction ledger facts by `effective_at`. `created_at` is never the
            /// confirmed-flow clock, and network/token contract must not partition
            /// wallet usage. The
            /// active key version must match the preflight snapshot. External-custody
            /// wallets cannot authorize outbound payments. Any concurrent lifecycle,
            /// key, or policy change
            /// fails closed rather than authorizing from stale preflight data.
            async fn authorize(&self, write: AuthorizationWrite<'_>) -> Result<IdempotentWrite<AuthorizationRecord>>;

            /// Returns usage in `budget_unit`: all matching `direction` reservations,
            /// without a `created_at` cutoff, plus confirmed same-direction ledger
            /// facts where `day_started_at <= effective_at <= now + bounded trusted
            /// settlement clock skew`. Scenario usage
            /// spans every current or historical policy (and wallet) for `scenario_id`;
            /// policy rotation must never reset it. Wallet usage spans every scenario. A complete
            /// [`crate::model::AssetIdentity`] must never be the grouping key. Effective
            /// times beyond the shared trusted-settlement clock-skew bound, or any
            /// matching `consumed` intent without its durable same-direction ledger fact,
            /// are invariant violations and must fail closed. Facts inside the bound are
            /// included immediately so confirmed spending cannot bypass a budget while
            /// the application clock catches up.
            async fn budget_snapshot(
                &self,
                scenario_id: &str,
                wallet_id: &str,
                direction: PaymentDirection,
                budget_unit: &BudgetUnit,
                day_started_at: i64,
                now: i64,
            ) -> Result<BudgetSnapshot>;

            /// Atomically records an inbound intent, reserves the policy's inbound
            /// sales/receipt budget, and appends its immutable challenge event.
            /// The transaction must lock/reload and revalidate the guard's client,
            /// scenario, offer, policy, policy-wallet membership, and wallet enabled
            /// state, ownership, revision/content, resource/origin,
            /// price/asset/protocol, directional limits, current inbound usage, and
            /// canonical recipient. Replays are accepted only when the stored request
            /// hash is identical.
            async fn record_challenge(&self, write: ChallengeWrite<'_>) -> Result<IdempotentWrite<PaymentIntent>>;

            /// Resolves a challenge only inside the authenticated scenario boundary.
            async fn get_for_challenge(&self, scenario_id: &str, challenge_hash: &str)
                -> Result<Option<PaymentIntent>>;

            /// Atomically binds a verified credential and advances the inbound intent.
            async fn verify_incoming(&self, write: VerificationWrite<'_>) -> Result<PaymentIntent>;

            async fn get(&self, intent_id: &str) -> Result<Option<PaymentIntent>>;

            /// Ownership-aware lookup used after authenticating the calling client.
            async fn get_for_scenario(&self, scenario_id: &str, intent_id: &str) -> Result<Option<PaymentIntent>>;

            /// Atomically advances a non-financial payment state and appends its
            /// operation events/outbox notifications. Implementations must reject
            /// `Signed`, `Settled`, any increase in settled money, and every
            /// reservation mutation except `Reserved -> Released` on `Failed` or
            /// `Expired`. Payment signatures and confirmed funds have dedicated
            /// transaction ports and cannot be expressed here.
            async fn commit_progress(&self, write: PaymentProgressWrite<'_>) -> Result<PaymentIntent>;

            /// Atomically records one idempotent outbound x402 merchant-response hint.
            /// A byte-for-byte equivalent semantic hint is a replay; another hint for
            /// the same intent is a conflict. Only independently confirmed RPC
            /// evidence may subsequently call [`Self::commit_settlement`].
            async fn record_outbound_evidence(
                &self,
                write: OutboundEvidenceWrite<'_>,
            ) -> Result<IdempotentWrite<PaymentIntent>>;

            async fn list(&self, query: &PaymentQuery) -> Result<Page<PaymentIntent>>;

            /// Claims due Signed/Submitted/Pending/Unknown recovery work and expired
            /// Created challenges. Leases must be persisted before rows are returned;
            /// Created rows are eligible only when their durable `expires_at` is due.
            async fn claim_reconciliation(&self, request: &ReconciliationLeaseRequest) -> Result<Vec<PaymentIntent>>;

            async fn renew_reconciliation_lease(&self, intent_id: &str, owner: &str, lease_until: i64) -> Result<()>;

            async fn release_reconciliation_lease(
                &self,
                intent_id: &str,
                owner: &str,
                next_attempt_at: i64,
            ) -> Result<()>;

            /// Commits settlement atomically. Any reserved intent must become
            /// `consumed` together with at least one matching, idempotently posted
            /// same-direction ledger fact. A consumed intent without that fact, or a replay
            /// whose ledger content differs, is an invariant error and fails closed.
            async fn commit_settlement(&self, write: SettlementWrite<'_>) -> Result<PaymentIntent>;
        }

        pub(crate) fn validate_outbound_evidence_transition(
            existing: &PaymentIntent,
            candidate: &PaymentIntent,
            expected_updated_at: i64,
            event: &PaymentEvent,
            evidence_hash: &str,
        ) -> Result<()> {
            let hash_valid = evidence_hash.len() == 64 && evidence_hash.bytes().all(|byte| byte.is_ascii_hexdigit());
            let payload = event.payload.get("merchant_response");
            let payload_bound = event.payload.get("evidence_hash").and_then(serde_json::Value::as_str)
                == Some(evidence_hash)
                && payload
                    .and_then(|value| value.get("success"))
                    .and_then(serde_json::Value::as_bool)
                    == Some(true)
                && payload
                    .and_then(|value| value.get("transaction"))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| !value.is_empty())
                && payload
                    .and_then(|value| value.get("network"))
                    .and_then(serde_json::Value::as_str)
                    == Some(existing.asset.network.as_str())
                && payload
                    .and_then(|value| value.get("payer"))
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|value| !value.is_empty());
            if !hash_valid
                || existing.updated_at != expected_updated_at
                || existing.direction != PaymentDirection::Outbound
                || existing.protocol != "x402"
                || !matches!(existing.status, PaymentStatus::Signed | PaymentStatus::Unknown)
                || candidate.status != PaymentStatus::PendingSettlement
                || !existing.same_immutable_identity_as(candidate)
                || candidate.updated_at <= expected_updated_at
                || candidate.reservation_status != existing.reservation_status
                || candidate.reserved_amount != existing.reserved_amount
                || existing.reservation_status != crate::model::ReservationStatus::Reserved
                || existing.reserved_amount != existing.amount
                || candidate.settled_amount != existing.settled_amount
                || candidate.tx_hash != existing.tx_hash
                || candidate.settlement_ref != existing.settlement_ref
                || candidate.settled_at != existing.settled_at
                || candidate.next_reconcile_at.is_none()
                || event.intent_id != existing.id
                || event.event_type != crate::model::PaymentEventType::Submitted
                || event.attempt_no != 0
                || event.signature_ref.is_some()
                || event.tx_hash.is_some()
                || event.created_at != candidate.updated_at
                || !payload_bound
            {
                return Err(crate::Error::PaymentConflict(
                    "outbound merchant evidence transition is invalid".to_owned(),
                ));
            }
            Ok(())
        }

        pub(crate) fn outbound_evidence_replay_matches(
            event: &PaymentEvent,
            intent_id: &str,
            evidence_hash: &str,
        ) -> bool {
            event.intent_id == intent_id
                && event.event_type == crate::model::PaymentEventType::Submitted
                && event.attempt_no == 0
                && event.signature_ref.is_none()
                && event.tx_hash.is_none()
                && event.payload.get("evidence_hash").and_then(serde_json::Value::as_str) == Some(evidence_hash)
        }
    }

    mod ledger {
        use async_trait::async_trait;

        use crate::{
            model::{PaymentDirection, PaymentLedgerEntry},
            Result,
        };

        use super::{IdempotentWrite, OutboxMessage, Page, PageRequest};

        #[derive(Clone, Debug, Default, PartialEq, Eq)]
        pub struct LedgerQuery {
            pub scenario_id: Option<String>,
            pub wallet_id: Option<String>,
            pub intent_id: Option<String>,
            pub direction: Option<PaymentDirection>,
            pub network: Option<String>,
            pub asset: Option<String>,
            pub from_effective_at: Option<i64>,
            pub to_effective_at: Option<i64>,
            pub page: PageRequest,
        }

        /// One append-only ledger transaction. Outbox messages are committed in the
        /// same database transaction as all entries.
        pub struct LedgerWrite<'a> {
            pub entries: &'a [PaymentLedgerEntry],
            pub outbox: &'a [OutboxMessage],
        }

        #[async_trait]
        pub trait LedgerStorage: Send + Sync {
            /// Appends one confirmed financial fact. The same `(wallet_id,
            /// posting_key)` and identical content is a replay; different content is a
            /// conflict. Multiple facts sharing one transaction/reference require a
            /// trusted, canonical `effect_ref`; without one, storage treats the
            /// evidence as a single fact. A reversal transaction must lock its original row, apply
            /// [`PaymentLedgerEntry::validate_reversal_of`], and reject a second
            /// reversal of the same original.
            async fn append(&self, entry: &PaymentLedgerEntry) -> Result<IdempotentWrite<PaymentLedgerEntry>>;

            /// Atomically appends a transfer pair, reversal batch, or deposit batch and
            /// its externally visible notifications. Every reversal has the same
            /// exact-wallet-fact and at-most-once requirements as [`Self::append`].
            async fn append_batch(&self, write: LedgerWrite<'_>) -> Result<Vec<IdempotentWrite<PaymentLedgerEntry>>>;

            async fn get(&self, ledger_id: &str) -> Result<Option<PaymentLedgerEntry>>;

            async fn find_posting(&self, wallet_id: &str, posting_key: &str) -> Result<Option<PaymentLedgerEntry>>;

            async fn list(&self, query: &LedgerQuery) -> Result<Page<PaymentLedgerEntry>>;
        }
    }

    mod outbox {
        use async_trait::async_trait;
        use serde_json::Value;

        use crate::Result;

        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum OutboxStatus {
            Pending,
            Delivered,
            DeadLetter,
        }

        impl OutboxStatus {
            #[must_use]
            pub const fn as_str(self) -> &'static str {
                match self {
                    Self::Pending => "pending",
                    Self::Delivered => "delivered",
                    Self::DeadLetter => "dead_letter",
                }
            }
        }

        /// Closed delivery families used to partition one durable outbox between
        /// independent workers. Callers cannot supply arbitrary SQL fragments or
        /// destination prefixes.
        #[derive(Clone, Copy, Debug, PartialEq, Eq)]
        pub enum OutboxDeliveryClass {
            /// Administrator-facing MQTT notifications for one client.
            ApprovalRequired,
            /// Durable payment lifecycle events for application/provider dispatchers.
            PaymentEvents,
        }

        impl OutboxDeliveryClass {
            pub(crate) const fn as_str(self) -> &'static str {
                match self {
                    Self::ApprovalRequired => "approval_required",
                    Self::PaymentEvents => "payment_events",
                }
            }

            /// Mirrors the controlled predicates implemented by SQL adapters.
            #[must_use]
            pub fn matches(self, message: &OutboxMessage) -> bool {
                match self {
                    Self::ApprovalRequired => {
                        message.event_type == "approval_required"
                            && destination_has_nonempty_id(&message.destination, "client:", ".approvals")
                    }
                    Self::PaymentEvents => {
                        destination_has_nonempty_id(&message.destination, "client:", ".payments")
                            || destination_has_nonempty_id(&message.destination, "scenario:", ".payments")
                    }
                }
            }
        }

        fn destination_has_nonempty_id(destination: &str, prefix: &str, suffix: &str) -> bool {
            destination
                .strip_prefix(prefix)
                .and_then(|value| value.strip_suffix(suffix))
                .is_some_and(|identifier| !identifier.is_empty())
        }

        /// Durable message inserted in the same transaction as its aggregate change.
        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct OutboxMessage {
            pub id: String,
            pub aggregate_type: String,
            pub aggregate_id: String,
            pub event_type: String,
            pub destination: String,
            /// Business run that produced this event. Together with
            /// `destination` and `event_type` this is the durable delivery
            /// identity; retries increment `attempt_count` without inventing
            /// another business key.
            pub run_id: String,
            pub payload: Value,
            pub status: OutboxStatus,
            pub attempt_count: u32,
            pub available_at: i64,
            pub lease_owner: Option<String>,
            pub lease_expires_at: Option<i64>,
            pub last_error: Option<String>,
            pub created_at: i64,
            pub delivered_at: Option<i64>,
        }

        #[derive(Clone, Debug, PartialEq, Eq)]
        pub struct OutboxLeaseRequest {
            pub owner: String,
            pub class: OutboxDeliveryClass,
            pub now: i64,
            pub lease_until: i64,
            pub limit: u32,
        }

        /// Persistent delivery queue. `PostgreSQL` implementations should claim with
        /// `FOR UPDATE SKIP LOCKED`; `SQLite` implementations use a short write
        /// transaction and conditional lease update.
        #[async_trait]
        pub trait OutboxStorage: Send + Sync {
            /// Claims only rows in `request.class`. A worker must never lease, mutate,
            /// or consume another delivery family's rows.
            async fn claim(&self, request: &OutboxLeaseRequest) -> Result<Vec<OutboxMessage>>;

            async fn mark_delivered(&self, message_id: &str, owner: &str, delivered_at: i64) -> Result<()>;

            async fn reschedule(&self, message_id: &str, owner: &str, available_at: i64, error: &str) -> Result<()>;

            async fn mark_dead_letter(&self, message_id: &str, owner: &str, error: &str) -> Result<()>;
        }
    }

    // Correctness-sensitive payment state, approval, event, ledger, and outbox
    // ports expose aggregate transactions rather than generic row mutation.

    use super::{IdempotentWrite, Page, PageRequest, PaymentWallet};

    pub use approval::{
        ApprovalConsumption, ApprovalIntentTransition, ApprovalReservationGuard, ApprovalStorage, ApprovalWrite,
    };
    pub use event::EventStorage;
    pub(crate) use intent::{outbound_evidence_replay_matches, validate_outbound_evidence_transition};
    pub use intent::{
        AuthorizationAction, AuthorizationGuard, AuthorizationRecord, AuthorizationWrite, BudgetSnapshot,
        ChallengeIssuanceGuard, ChallengeWrite, OutboundEvidenceWrite, PaymentProgressWrite, PaymentQuery,
        PaymentStorage, ReconciliationLeaseRequest, SettlementWrite, VerificationWrite,
    };
    pub use ledger::{LedgerQuery, LedgerStorage, LedgerWrite};
    pub use outbox::{OutboxDeliveryClass, OutboxLeaseRequest, OutboxMessage, OutboxStatus, OutboxStorage};
}

pub use catalog::{
    ApproverStorage, ClientStorage, EncryptedWalletKeyVersion, OfferStorage, PaymentWallet, PolicyStorage,
    ScenarioConfiguration, ScenarioStorage, WalletBalanceRecord, WalletBalanceStorage, WalletCustody, WalletKeyStatus,
    WalletProvisioningConfig, WalletStorage,
};
pub use signing::{
    IntentBoundPaymentSignatureRequest, OutboundSigningAuthorization, OutboundSigningIdentity, OutboundSigningRead,
    PersistedSignedPayment, SigningGuard, SigningStorage, SigningWrite, WalletPaymentSignature, WalletPaymentSigner,
};
pub(crate) use transaction::{outbound_evidence_replay_matches, validate_outbound_evidence_transition};
pub use transaction::{
    ApprovalConsumption, ApprovalIntentTransition, ApprovalReservationGuard, ApprovalStorage, ApprovalWrite,
    AuthorizationAction, AuthorizationGuard, AuthorizationRecord, AuthorizationWrite, BudgetSnapshot,
    ChallengeIssuanceGuard, ChallengeWrite, EventStorage, LedgerQuery, LedgerStorage, LedgerWrite,
    OutboundEvidenceWrite, OutboxDeliveryClass, OutboxLeaseRequest, OutboxMessage, OutboxStatus, OutboxStorage,
    PaymentProgressWrite, PaymentQuery, PaymentStorage, ReconciliationLeaseRequest, SettlementWrite, VerificationWrite,
};

/// Provider-neutral handles for every durable payment domain port.
///
/// The composition root obtains these handles only from [`IStorage::ports`];
/// handlers never instantiate or downcast a SQL adapter.
#[derive(Clone)]
pub struct StoragePorts {
    pub clients: Arc<dyn ClientStorage>,
    pub scenarios: Arc<dyn ScenarioStorage>,
    pub approvers: Arc<dyn ApproverStorage>,
    pub policies: Arc<dyn PolicyStorage>,
    pub offers: Arc<dyn OfferStorage>,
    pub wallets: Arc<dyn WalletStorage>,
    pub wallet_balances: Arc<dyn WalletBalanceStorage>,
    pub payments: Arc<dyn PaymentStorage>,
    pub approvals: Arc<dyn ApprovalStorage>,
    pub events: Arc<dyn EventStorage>,
    pub ledger: Arc<dyn LedgerStorage>,
    pub outbox: Arc<dyn OutboxStorage>,
    pub signing: Arc<dyn SigningStorage>,
}

/// Versioned local-wallet custody operations backed by the same authoritative
/// SQL component. Raw private-key material never leaves this boundary.
#[async_trait]
pub trait WalletKeyManager: WalletPaymentSigner {
    async fn create_local(
        &self,
        wallet_id: &str,
        private_key: &[u8],
        config: &WalletProvisioningConfig,
        created_at: i64,
    ) -> Result<Wallet>;

    async fn rotate(&self, wallet_id: &str, private_key: &[u8], activated_at: i64) -> Result<Wallet>;
}

/// Unique lifecycle and domain contract implemented by every SQL provider.
///
/// Providers are constructed and consumed exclusively through
/// [`StorageFactory`]. The returned domain ports all share the same pool and
/// transaction authority.
pub trait IStorage: Send + Sync {
    fn provider(&self) -> StorageProvider;

    fn init(&self) -> StorageLifecycleFuture;

    /// Checks whether the authoritative SQL provider can serve a trivial
    /// query. Management readiness uses this signal to stop routing new
    /// payment work when durable authority is unavailable.
    fn ping(&self) -> StorageHealthFuture;

    fn close(&self) -> StorageCloseFuture;

    fn ports(self: Arc<Self>) -> StoragePorts;

    /// Builds the versioned local-wallet custody port for this SQL instance.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured wallet key ring cannot be loaded.
    fn wallet_key_manager(self: Arc<Self>, config: &WalletKeyRingConfig) -> Result<Arc<dyn WalletKeyManager>>;

    /// Builds the intent-bound payment signer for this SQL instance.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured wallet key ring cannot be loaded.
    fn wallet_payment_signer(self: Arc<Self>, config: &WalletKeyRingConfig) -> Result<Arc<dyn WalletPaymentSigner>>;
}

/// Initialization performs `SQLx` migration batches whose executor futures are
/// intentionally awaited only by the single-threaded composition phase.
pub type StorageLifecycleFuture = Pin<Box<dyn Future<Output = Result<()>> + 'static>>;
pub type StorageHealthFuture = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;
pub type StorageCloseFuture = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;

struct RegisteredStorage {
    config: StorageConfig,
    component: Arc<dyn IStorage>,
}

/// Sole factory and registry for authoritative storage components.
#[derive(Default)]
pub struct StorageFactory {
    registry: Mutex<BTreeMap<String, RegisteredStorage>>,
}

impl StorageFactory {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Selects a provider, registers it, initializes it, and rolls registration
    /// back on failure. A compatible instance key is reused by identity.
    ///
    /// # Errors
    ///
    /// Returns an error when configuration or secrets are invalid, the key is
    /// already bound to different configuration, or provider initialization
    /// fails.
    pub async fn create(&self, config: &StorageConfig) -> Result<Arc<dyn IStorage>> {
        validate_storage_instance_key(&config.instance_key)?;
        let mut registry = self.registry.lock().await;
        if let Some(registered) = registry.get(&config.instance_key) {
            return if registered.config == *config {
                Ok(Arc::clone(&registered.component))
            } else {
                Err(Error::Config(format!(
                    "storage instance {} is already registered with different configuration",
                    config.instance_key
                )))
            };
        }

        let component: Arc<dyn IStorage> = match config.provider {
            StorageProvider::Sqlite => Arc::new(SqliteStorage::from_config(&config.sqlite)?),
            StorageProvider::Postgres => {
                let dsn = read_secret_ref(&config.postgres.dsn, "PostgreSQL payment DSN", MAX_STORAGE_DSN_BYTES)?;
                Arc::new(PostgresStorage::from_config(
                    &dsn,
                    config.postgres.max_connections,
                    config.postgres.connect_timeout,
                )?)
            }
        };
        registry.insert(
            config.instance_key.clone(),
            RegisteredStorage {
                config: config.clone(),
                component: Arc::clone(&component),
            },
        );
        if let Err(error) = component.init().await {
            registry.remove(&config.instance_key);
            drop(registry);
            let _ = component.close().await;
            return Err(error);
        }
        Ok(component)
    }

    /// Returns an initialized authoritative storage by stable instance key.
    ///
    /// # Errors
    ///
    /// Returns an error when the key is invalid or no storage is registered.
    pub async fn get(&self, instance_key: &str) -> Result<Arc<dyn IStorage>> {
        validate_storage_instance_key(instance_key)?;
        self.registry
            .lock()
            .await
            .get(instance_key)
            .map(|registered| Arc::clone(&registered.component))
            .ok_or_else(|| Error::NotFound(format!("storage instance {instance_key}")))
    }

    /// Unregisters before closing. Repeated close calls are idempotent.
    ///
    /// # Errors
    ///
    /// Returns an error when the key is invalid or the provider cannot close.
    pub async fn close(&self, instance_key: &str) -> Result<()> {
        validate_storage_instance_key(instance_key)?;
        let component = self
            .registry
            .lock()
            .await
            .remove(instance_key)
            .map(|registered| registered.component);
        if let Some(component) = component {
            component.close().await?;
        }
        Ok(())
    }

    /// Unregisters all instances before closing every provider. All closes are
    /// attempted and the first error is returned.
    ///
    /// # Errors
    ///
    /// Returns the first provider shutdown failure after attempting every
    /// registered storage instance.
    pub async fn shutdown_all(&self) -> Result<()> {
        let components = {
            let mut registry = self.registry.lock().await;
            std::mem::take(&mut *registry)
                .into_values()
                .map(|registered| registered.component)
                .collect::<Vec<_>>()
        };
        let mut first_error = None;
        for component in components {
            if let Err(error) = component.close().await {
                first_error.get_or_insert(error);
            }
        }
        first_error.map_or(Ok(()), Err)
    }
}

fn validate_storage_instance_key(value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(Error::Config("storage.instance_key is invalid".to_owned()));
    }
    Ok(())
}

/// Bounded, offset-based page request used by management queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRequest {
    pub offset: u64,
    pub limit: u32,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self { offset: 0, limit: 10 }
    }
}

/// One page plus the total number of matching durable records.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
}

/// Outcome of an insert guarded by a durable domain replay identity.
///
/// Implementations must return `Replay` only when the stored request hash and
/// material fields match. Reusing the identity for different content is an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdempotentWrite<T> {
    Inserted(T),
    Replay(T),
}

#[cfg(test)]
#[path = "../../tests/storage/storage_test.rs"]
mod wallet_tests;
