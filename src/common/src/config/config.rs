#[path = "validation.rs"]
mod validation;
#[cfg(test)]
use validation::ConfigValidation;
#[path = "constants.rs"]
pub mod constants;
pub use constants::*;
// Strongly typed process configuration.
//
// Precedence is: built-in defaults < YAML file < `AGENTVAULT__...`
// environment variables. Double underscores address nested fields, for
// example `AGENTVAULT__MGMT__OTEL__ENABLED=true`.
//
// Sensitive leaves use the same canonical field-path names. Their values are
// resolved by [`SecretRefConfig`] and are deliberately filtered out of the
// serializable configuration source.

use std::{
    collections::BTreeMap, collections::BTreeSet, collections::HashMap, net::IpAddr, path::Path, time::Duration,
};

use ::config::{Config as ConfigLoader, File};
use serde::{Deserialize, Serialize};

use crate::{model::wallet::WalletSelectionStrategy, model::Amount, Error, Result};

/// Complete `AgentVault` configuration. Definitions intentionally follow the
/// same outer-to-inner order as the YAML document.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub mgmt: ManagementConfig,
    pub logging: LoggingConfig,
    /// Process-wide authoritative persistence shared by payment services.
    pub storage: StorageConfig,
    /// Process-wide non-authoritative acceleration shared by runtime services.
    pub cache: CacheConfig,
    /// The only AgentVault-specific root. Directional payment adapters and
    /// their external authorities live below this boundary.
    pub agentvault: AgentVaultConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub enabled: bool,
    pub service_name: String,
    pub host: IpAddr,
    pub port: u16,
    pub context_path: String,
    pub authguard: AuthGuardIntegrationConfig,
    #[serde(with = "humantime_serde")]
    pub shutdown_timeout: Duration,
    pub request: RequestConfig,
}

/// Optional `AuthGuard` edge integration for HTTP callers.
///
/// `AgentVault` never validates an identity-provider credential or evaluates an
/// IAM policy. When enabled, its workload adapter verifies the short-lived
/// signed access context emitted by `AuthGuard` and extracts only the principal
/// identifier needed by the payment domain. Disabling the integration
/// intentionally makes HTTP administration and payment routes unrestricted;
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthGuardIntegrationConfig {
    pub enabled: bool,
    pub direct_context_hmac_key: SecretRefConfig,
    pub scope_resolver: AuthGuardScopeResolverConfig,
}

/// `AuthGuard`'s internal gRPC resolver for opaque scope-token delivery.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AuthGuardScopeResolverConfig {
    pub enabled: bool,
    pub target: String,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RequestConfig {
    pub max_body_bytes: usize,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ManagementConfig {
    pub enabled: bool,
    pub host: IpAddr,
    pub port: u16,
    pub context_path: String,
    pub health: HealthConfig,
    pub metrics: MetricsConfig,
    pub otel: OtelConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct HealthConfig {
    pub liveness_path: String,
    pub readiness_path: String,
    #[serde(with = "humantime_serde")]
    pub probe_timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MetricsConfig {
    pub enabled: bool,
    pub path: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct OtelConfig {
    pub enabled: bool,
    pub endpoint: String,
    pub protocol: String,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    pub sample_rate: f64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LoggingConfig {
    pub mode: String,
    pub level: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StorageProvider {
    #[default]
    Sqlite,
    Postgres,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    pub provider: StorageProvider,
    pub sqlite: SqliteStorageConfig,
    pub postgres: PostgresStorageConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SqliteStorageConfig {
    /// Absolute durable database path.
    pub database_file: String,
    pub max_connections: u32,
    #[serde(with = "humantime_serde")]
    pub busy_timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct PostgresStorageConfig {
    /// `PostgreSQL` connection URL source. The URL itself is never a
    /// serializable field. Rust `sqlx` accepts `postgresql://`/`postgres://`
    /// URLs; JDBC URLs belong to JVM drivers and are intentionally rejected.
    pub url: SecretRefConfig,
    pub max_connections: u32,
    #[serde(with = "humantime_serde")]
    pub connect_timeout: Duration,
}

/// Reference to a secret value. Exactly one source must be set when used.
///
/// This type intentionally has no inline value variant so serialization and
/// debug output can expose only a file path or environment variable name.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct SecretRefConfig {
    pub file: String,
    pub env: String,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CacheProvider {
    #[default]
    Memory,
    Redis,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RedisCacheMode {
    #[default]
    Standalone,
    Cluster,
}

/// Non-authoritative cache configuration. Budget reservations, approval grant
/// consumption, settlement state, and ledger entries must never depend on it.
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    pub provider: CacheProvider,
    pub memory: MemoryCacheConfig,
    pub redis: RedisCacheConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct MemoryCacheConfig {
    pub max_entries: usize,
    pub max_value_bytes: usize,
    #[serde(with = "humantime_serde")]
    pub default_ttl: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct RedisCacheConfig {
    pub mode: RedisCacheMode,
    /// Redis URLs may contain credentials. Every URL is therefore resolved
    /// exclusively from an environment-backed [`SecretRefConfig`]. Standalone
    /// mode requires one URL; cluster mode accepts one or more seed URLs.
    pub urls: Vec<SecretRefConfig>,
    pub key_prefix: String,
    pub max_value_bytes: usize,
    #[serde(with = "humantime_serde")]
    pub connect_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub operation_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub default_ttl: Duration,
    pub retry_attempts: u32,
    #[serde(with = "humantime_serde")]
    pub min_retry_wait: Duration,
    #[serde(with = "humantime_serde")]
    pub max_retry_wait: Duration,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            service_name: "agentvault".to_owned(),
            host: IpAddr::from([127, 0, 0, 1]),
            port: 8080,
            context_path: "/api/v1".to_owned(),
            authguard: AuthGuardIntegrationConfig::default(),
            shutdown_timeout: Duration::from_secs(15),
            request: RequestConfig::default(),
        }
    }
}

impl Default for AuthGuardIntegrationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            direct_context_hmac_key: SecretRefConfig {
                file: String::new(),
                env: "AGENTVAULT__SERVER__AUTHGUARD__DIRECT_CONTEXT_HMAC_KEY".to_owned(),
            },
            scope_resolver: AuthGuardScopeResolverConfig::default(),
        }
    }
}

impl Default for AuthGuardScopeResolverConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            target: "http://127.0.0.1:8081".to_owned(),
            timeout: Duration::from_secs(2),
        }
    }
}

impl Default for RequestConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: 16 * 1024,
            timeout: Duration::from_secs(5),
        }
    }
}

impl Default for ManagementConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            host: IpAddr::from([127, 0, 0, 1]),
            port: 9091,
            context_path: "/".to_owned(),
            health: HealthConfig::default(),
            metrics: MetricsConfig::default(),
            otel: OtelConfig::default(),
        }
    }
}

impl Default for HealthConfig {
    fn default() -> Self {
        Self {
            liveness_path: "/healthz".to_owned(),
            readiness_path: "/readyz".to_owned(),
            probe_timeout: Duration::from_secs(1),
        }
    }
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            path: "/metrics".to_owned(),
        }
    }
}

impl Default for OtelConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://127.0.0.1:4317".to_owned(),
            protocol: "grpc".to_owned(),
            timeout: Duration::from_secs(5),
            sample_rate: 1.0,
        }
    }
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            mode: "HUMAN".to_owned(),
            level: "info".to_owned(),
        }
    }
}

impl Default for SqliteStorageConfig {
    fn default() -> Self {
        Self {
            database_file: "/var/lib/agentvault/agentvault.sqlite3".to_owned(),
            max_connections: 1,
            busy_timeout: Duration::from_secs(5),
        }
    }
}

impl Default for PostgresStorageConfig {
    fn default() -> Self {
        Self {
            url: SecretRefConfig {
                file: String::new(),
                env: "AGENTVAULT__STORAGE__POSTGRES__URL".to_owned(),
            },
            max_connections: 16,
            connect_timeout: Duration::from_secs(5),
        }
    }
}

impl Default for MemoryCacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_value_bytes: 1024 * 1024,
            default_ttl: Duration::from_mins(5),
        }
    }
}

impl Default for RedisCacheConfig {
    fn default() -> Self {
        Self {
            mode: RedisCacheMode::Standalone,
            urls: vec![SecretRefConfig {
                file: String::new(),
                env: "AGENTVAULT__CACHE__REDIS__URLS".to_owned(),
            }],
            key_prefix: "agentvault".to_owned(),
            max_value_bytes: 1024 * 1024,
            connect_timeout: Duration::from_secs(3),
            operation_timeout: Duration::from_secs(2),
            default_ttl: Duration::from_mins(5),
            retry_attempts: 5,
            min_retry_wait: Duration::from_millis(100),
            max_retry_wait: Duration::from_secs(2),
        }
    }
}

/// Enterprise `AgentVault` runtime configuration.
///
/// Business authorization, budget, recipient, network, asset, and pricing
/// rules deliberately live in the persistent Policy and Price stores rather
/// than process configuration.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AgentVaultConfig {
    pub payer: PayerConfig,
    pub payee: PayeeConfig,
    /// Shared operator-owned chain registry keyed by canonical CAIP-2 network.
    pub chains: BTreeMap<String, ChainConfig>,
    /// Versioned KEKs used only to envelope-encrypt persisted Wallet EOA keys.
    pub key_management: WalletKeyRingConfig,
    /// Independent application encryption for short-lived durable recovery
    /// material. This never contains or encrypts Wallet EOA private keys.
    pub recovery_encryption: RecoveryEncryptionConfig,
    pub approval: ApprovalRuntimeConfig,
    pub reconciliation: ReconciliationConfig,
    pub wallet_balance_sync: WalletBalanceSyncConfig,
}

impl AgentVaultConfig {
    #[must_use]
    pub const fn enabled(&self) -> bool {
        self.payer.enabled || self.payee.enabled
    }
}

/// Outbound payment adapters. A payer consumes remote challenges and signs
/// locally; it does not configure the merchant's facilitator/provider.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PayerConfig {
    pub enabled: bool,
    pub wallet_selection: WalletSelectionConfig,
    pub x402: PayerX402Config,
    pub mpp: PayerMppConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WalletSelectionConfig {
    pub strategy: WalletSelectionStrategy,
}

/// Inbound payment adapters. Each provider is colocated with the protocol
/// whose credentials it verifies and settles.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PayeeConfig {
    pub enabled: bool,
    pub x402: PayeeX402Config,
    pub mpp: PayeeMppConfig,
}

/// Indirect references for versioned Wallet key-encryption keys. EOA private
/// keys themselves are encrypted in the SQL Wallet aggregate.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WalletKeyRingConfig {
    pub active_key_id: String,
    pub keys: BTreeMap<String, SecretRefConfig>,
    /// Lifetime of a one-use browser-to-Vault wallet-key import session.
    #[serde(with = "humantime_serde")]
    pub import_session_ttl: Duration,
}

/// Versioned keys protecting durable credential and approval recovery
/// envelopes. These keys are deliberately distinct from Wallet KEKs.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecoveryEncryptionConfig {
    pub active_key_id: String,
    pub keys: BTreeMap<String, SecretRefConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PayerX402Config {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub max_challenge_ttl: Duration,
    pub assets: Vec<X402AssetConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PayeeX402Config {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub max_challenge_ttl: Duration,
    pub assets: Vec<X402AssetConfig>,
    pub facilitator: ExternalProviderConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct X402AssetConfig {
    pub network: String,
    pub contract: String,
    pub symbol: String,
    pub decimals: u8,
    pub fee_payer: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PayerMppConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub max_challenge_ttl: Duration,
    pub assets: Vec<MppAssetConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct PayeeMppConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub max_challenge_ttl: Duration,
    pub realm: String,
    pub challenge_secret: SecretRefConfig,
    pub assets: Vec<MppAssetConfig>,
    pub provider: ExternalProviderConfig,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct MppAssetConfig {
    pub network: String,
    pub currency: String,
    pub symbol: String,
    pub token_contract: Option<String>,
    pub decimals: u8,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ChainFamily {
    #[default]
    Evm,
    Solana,
}

/// One shared chain boundary used by payer balance/signing checks and payee
/// settlement reconciliation. Protocol challenges cannot override it.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChainConfig {
    pub family: ChainFamily,
    pub rpc: ChainRpcConfig,
    pub minimum_native_balance: Amount,
    pub confirmation_depth: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ChainRpcConfig {
    pub endpoint: String,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    pub max_response_bytes: usize,
    pub allow_insecure_loopback: bool,
    pub secret_headers: BTreeMap<String, SecretHeaderConfig>,
}

/// HTTP configuration for a hosted facilitator or payment provider.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ExternalProviderConfig {
    pub id: String,
    pub base_url: String,
    pub verify_path: String,
    pub settle_path: String,
    #[serde(with = "humantime_serde")]
    pub timeout: Duration,
    pub max_response_bytes: usize,
    /// Plain HTTP is limited to an explicitly configured loopback endpoint.
    pub allow_insecure_loopback: bool,
    /// Header values are indirect secret references; literal values are not
    /// representable by this configuration model.
    pub secret_headers: BTreeMap<String, SecretHeaderConfig>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecretHeaderConfig {
    pub source: SecretRefConfig,
    /// Non-secret scheme prefix such as `Bearer `.
    pub prefix: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalRuntimeConfig {
    #[serde(with = "humantime_serde")]
    pub request_ttl: Duration,
    #[serde(with = "humantime_serde")]
    pub grant_ttl: Duration,
    #[serde(with = "humantime_serde")]
    pub expiry_interval: Duration,
    pub expiry_batch_size: usize,
    pub jwt: ApprovalJwtConfig,
}

/// Internal one-time approval JWT configuration.
///
/// Signing key bytes are never represented inline. Each key is loaded from an
/// indirect secret reference, and retired key identifiers remain configured
/// until every JWT they signed has expired or been consumed.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ApprovalJwtConfig {
    pub issuer: String,
    pub audience: String,
    pub active_key_id: String,
    pub signing_keys: BTreeMap<String, SecretRefConfig>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ReconciliationConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    #[serde(with = "humantime_serde")]
    pub lease_ttl: Duration,
    pub batch_size: usize,
    #[serde(with = "humantime_serde")]
    pub initial_backoff: Duration,
    #[serde(with = "humantime_serde")]
    pub max_backoff: Duration,
}

/// Periodic, non-authoritative observations of managed wallet balances.
/// Authorization and signing never trust these snapshots; they remain an
/// operations, capacity, and alerting data set.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct WalletBalanceSyncConfig {
    pub enabled: bool,
    #[serde(with = "humantime_serde")]
    pub interval: Duration,
    pub batch_size: usize,
}

impl Default for WalletKeyRingConfig {
    fn default() -> Self {
        Self {
            active_key_id: "wallet-kek-v1".to_owned(),
            keys: BTreeMap::from([(
                "wallet-kek-v1".to_owned(),
                SecretRefConfig {
                    file: String::new(),
                    env: "AGENTVAULT__AGENTVAULT__KEY_MANAGEMENT__KEYS__WALLET_KEK_V1".to_owned(),
                },
            )]),
            import_session_ttl: Duration::from_mins(2),
        }
    }
}

impl Default for RecoveryEncryptionConfig {
    fn default() -> Self {
        Self {
            active_key_id: "recovery-v1".to_owned(),
            keys: BTreeMap::from([(
                "recovery-v1".to_owned(),
                SecretRefConfig {
                    file: String::new(),
                    env: "AGENTVAULT__AGENTVAULT__RECOVERY_ENCRYPTION__KEYS__RECOVERY_V1".to_owned(),
                },
            )]),
        }
    }
}

impl Default for PayerX402Config {
    fn default() -> Self {
        Self {
            enabled: false,
            max_challenge_ttl: Duration::from_mins(5),
            assets: Vec::new(),
        }
    }
}

impl Default for PayeeX402Config {
    fn default() -> Self {
        Self {
            enabled: false,
            max_challenge_ttl: Duration::from_mins(5),
            assets: Vec::new(),
            facilitator: ExternalProviderConfig::default(),
        }
    }
}

impl Default for X402AssetConfig {
    fn default() -> Self {
        Self {
            network: String::new(),
            contract: String::new(),
            symbol: "USDC".to_owned(),
            decimals: 6,
            fee_payer: None,
        }
    }
}

impl Default for PayerMppConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_challenge_ttl: Duration::from_mins(5),
            assets: Vec::new(),
        }
    }
}

impl Default for PayeeMppConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_challenge_ttl: Duration::from_mins(5),
            realm: String::new(),
            challenge_secret: SecretRefConfig {
                file: String::new(),
                env: "AGENTVAULT__AGENTVAULT__PAYEE__MPP__CHALLENGE_SECRET".to_owned(),
            },
            assets: Vec::new(),
            provider: ExternalProviderConfig::default(),
        }
    }
}

impl Default for ChainConfig {
    fn default() -> Self {
        Self {
            family: ChainFamily::default(),
            rpc: ChainRpcConfig::default(),
            minimum_native_balance: Amount::ZERO,
            confirmation_depth: 1,
        }
    }
}

impl Default for ChainRpcConfig {
    fn default() -> Self {
        Self {
            endpoint: String::new(),
            timeout: Duration::from_secs(5),
            max_response_bytes: 256 * 1024,
            allow_insecure_loopback: false,
            secret_headers: BTreeMap::new(),
        }
    }
}

impl Default for ExternalProviderConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            base_url: String::new(),
            verify_path: "verify".to_owned(),
            settle_path: "settle".to_owned(),
            timeout: Duration::from_secs(10),
            max_response_bytes: 256 * 1024,
            allow_insecure_loopback: false,
            secret_headers: BTreeMap::new(),
        }
    }
}

impl Default for ApprovalRuntimeConfig {
    fn default() -> Self {
        Self {
            request_ttl: Duration::from_mins(15),
            grant_ttl: Duration::from_mins(5),
            expiry_interval: Duration::from_secs(15),
            expiry_batch_size: 100,
            jwt: ApprovalJwtConfig::default(),
        }
    }
}

impl Default for ApprovalJwtConfig {
    fn default() -> Self {
        Self {
            issuer: DEFAULT_APPROVAL_JWT_ISSUER.to_owned(),
            audience: DEFAULT_APPROVAL_JWT_AUDIENCE.to_owned(),
            active_key_id: DEFAULT_APPROVAL_JWT_KEY_ID.to_owned(),
            signing_keys: BTreeMap::from([(
                DEFAULT_APPROVAL_JWT_KEY_ID.to_owned(),
                SecretRefConfig {
                    file: String::new(),
                    env: "AGENTVAULT__AGENTVAULT__APPROVAL__JWT__SIGNING_KEYS__APPROVAL_HS256_V1".to_owned(),
                },
            )]),
        }
    }
}

impl Default for ReconciliationConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_secs(15),
            lease_ttl: Duration::from_secs(45),
            batch_size: 100,
            initial_backoff: Duration::from_secs(5),
            max_backoff: Duration::from_mins(5),
        }
    }
}

impl Default for WalletBalanceSyncConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            interval: Duration::from_mins(3),
            batch_size: 1_000,
        }
    }
}

impl AppConfig {
    /// Loads YAML and applies `AGENTVAULT__...` overrides.
    ///
    /// # Errors
    ///
    /// Returns an error when the source cannot be loaded or deserialized.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let base = Self::load_file(path)?;
        let secret_variables = base.secret_environment_variables()?;
        let mut source = HashMap::new();
        for (key, value) in std::env::vars_os() {
            let Ok(key) = key.into_string() else {
                continue;
            };
            if !key.starts_with("AGENTVAULT__") {
                continue;
            }
            if secret_variables.contains(&key) || key == SECRETS_ENV_FILE_ENV {
                continue;
            }
            let value = value
                .into_string()
                .map_err(|_| Error::Config(format!("environment variable {key} must contain UTF-8 text")))?;
            source.insert(key, value);
        }
        Self::load_with_filtered_environment(base, source)
    }

    fn load_file(path: &Path) -> Result<Self> {
        ConfigLoader::builder()
            .add_source(File::from(path))
            .build()
            .and_then(ConfigLoader::try_deserialize::<Self>)
            .map_err(|error| Error::Config(error.to_string()))
    }

    #[cfg(test)]
    fn load_with_environment(path: &Path, source: HashMap<String, String>) -> Result<Self> {
        let base = Self::load_file(path)?;
        let secret_variables = base.secret_environment_variables()?;
        let source = source
            .into_iter()
            .filter(|(key, _)| !secret_variables.contains(key) && key != SECRETS_ENV_FILE_ENV)
            .collect();
        Self::load_with_filtered_environment(base, source)
    }

    fn load_with_filtered_environment(base: Self, source: HashMap<String, String>) -> Result<Self> {
        let mut tree = serde_json::to_value(base)?;
        // Stable ordering rejects conflicting parent/child overrides consistently.
        for (key, value) in source.into_iter().collect::<BTreeMap<_, _>>() {
            if let Some(path) = key.strip_prefix("AGENTVAULT__") {
                let parts = path.split("__").collect::<Vec<_>>();
                Self::apply_environment(&mut tree, &parts, value)?;
            }
        }
        let source: ::config::Value = serde_json::from_value(tree)?;
        source
            .try_deserialize::<Self>()
            .map_err(|error| Error::Config(error.to_string()))
    }

    fn apply_environment(node: &mut serde_json::Value, path: &[&str], value: String) -> Result<()> {
        let Some((part, tail)) = path.split_first() else {
            *node = serde_json::Value::String(value);
            return Ok(());
        };
        if part.is_empty() {
            return Err(Error::Config(
                "environment configuration path has an empty segment".to_owned(),
            ));
        }
        if node.is_null() {
            *node = if part.parse::<usize>().is_ok() {
                serde_json::Value::Array(Vec::new())
            } else {
                serde_json::json!({})
            };
        }
        if let Some(fields) = node.as_object_mut() {
            let key = fields
                .keys()
                .find(|key| key.eq_ignore_ascii_case(part))
                .cloned()
                .unwrap_or_else(|| part.to_ascii_lowercase());
            Self::apply_environment(fields.entry(key).or_insert(serde_json::Value::Null), tail, value)
        } else if let Some(items) = node.as_array_mut() {
            let index = part
                .parse::<usize>()
                .ok()
                .filter(|index| *index < 1024)
                .ok_or_else(|| {
                    Error::Config("environment configuration array index must be between 0 and 1023".to_owned())
                })?;
            if items.len() <= index {
                items.resize(index + 1, serde_json::Value::Null);
            }
            Self::apply_environment(&mut items[index], tail, value)
        } else {
            Err(Error::Config(format!(
                "environment configuration path traverses a scalar at {part}"
            )))
        }
    }

    fn secret_environment_variables(&self) -> Result<BTreeSet<String>> {
        let mut names = BTreeSet::new();
        let tree = serde_json::to_value(self)?;
        SecretRefConfig::collect_environment_variables(&tree, &mut names);
        Ok(names)
    }

    /// Validates the complete daemon configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe or internally inconsistent runtime configuration.
    pub fn validate(&self) -> Result<()> {
        self.validate_for_serve()
    }

    /// Returns the configured absolute authoritative `SQLite` database path.
    #[must_use]
    pub fn sqlite_database_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.storage.sqlite.database_file)
    }
}

#[cfg(test)]
#[path = "../../tests/config/config_test.rs"]
mod config_tests;

#[cfg(test)]
#[path = "../../tests/config/foundation_test.rs"]
mod foundation_tests;

#[cfg(test)]
#[path = "../../tests/config/payment_test.rs"]
mod payment_tests;

impl SecretRefConfig {
    // Walk maps and sequences using Serde's SecretRef schema to identify
    // references. No consuming module, field path or serialized key inventory
    // is maintained here.
    fn collect_environment_variables(value: &serde_json::Value, names: &mut BTreeSet<String>) {
        match value {
            serde_json::Value::Object(fields) => {
                if let Ok(reference) = Self::deserialize(value) {
                    if !reference.env.is_empty() {
                        names.insert(reference.env);
                    }
                }
                for child in fields.values() {
                    Self::collect_environment_variables(child, names);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    Self::collect_environment_variables(child, names);
                }
            }
            _ => {}
        }
    }
}
