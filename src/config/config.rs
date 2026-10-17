//! Strongly typed process configuration.
//!
//! Precedence is: built-in defaults < YAML file < `AGENTVAULT__...`
//! environment variables. Double underscores address nested fields, for
//! example `AGENTVAULT__MGMT__OTEL__ENABLED=true`.

use std::{collections::BTreeMap, net::IpAddr, path::Path, time::Duration};

use ::config::{Config as ConfigLoader, Environment, File};
use serde::{Deserialize, Serialize};

use crate::{model::Amount, Error, Result};

pub const DEFAULT_MQTT_TOPIC_PREFIX: &str = "agentvault/v1/clients";
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_CACHE_ENTRIES: usize = 1_000_000;
pub(crate) const MAX_CACHE_VALUE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_CACHE_REDIS_ENDPOINTS: usize = 32;
pub(crate) const MAX_CACHE_RETRIES: u32 = 32;
pub(crate) const MAX_CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_STORAGE_CONNECTIONS: u32 = 256;
const MAX_PROVIDER_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RECONCILIATION_BATCH_SIZE: usize = 10_000;
const MAX_CHALLENGE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const MAX_HEALTH_PROBE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_PROTOCOL_ASSETS: usize = 64;
const MAX_RPC_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const DEFAULT_APPROVAL_JWT_ISSUER: &str = "agentvault";
const DEFAULT_APPROVAL_JWT_AUDIENCE: &str = "agentvault:approval-grant";
const DEFAULT_APPROVAL_JWT_KEY_ID: &str = "approval-hs256-v1";

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
/// MQTT retains its independent client-id/ACL boundary.
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

/// Enterprise `AgentVault` runtime configuration.
///
/// Business authorization, budget, recipient, network, asset, and pricing
/// rules deliberately live in the persistent Policy and Offer stores rather
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
    pub transport: TransportConfig,
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
    pub x402: PayerX402Config,
    pub mpp: PayerMppConfig,
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
}

/// Versioned keys protecting durable credential and approval recovery
/// envelopes. These keys are deliberately distinct from Wallet KEKs.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct RecoveryEncryptionConfig {
    pub active_key_id: String,
    pub keys: BTreeMap<String, SecretRefConfig>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StorageProvider {
    #[default]
    Sqlite,
    Postgres,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct StorageConfig {
    /// Stable process-local registry key used by [`StorageFactory`].
    #[serde(default = "default_component_instance_key")]
    pub instance_key: String,
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
    /// `PostgreSQL` DSN source. The DSN itself is never a serializable field.
    pub dsn: SecretRefConfig,
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
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct CacheConfig {
    /// Stable process-local registry key used by [`CacheFactory`].
    #[serde(default = "default_component_instance_key")]
    pub instance_key: String,
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

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct TransportConfig {
    pub grpc: GrpcTransportConfig,
    pub local: LocalTransportConfig,
    pub mqtt: MqttTransportConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct GrpcTransportConfig {
    pub enabled: bool,
    pub host: IpAddr,
    pub port: u16,
    pub max_message_bytes: usize,
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalTransportConfig {
    pub enabled: bool,
    pub socket_path: String,
    pub max_connections: usize,
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MqttTransportConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub consumer_group: String,
    pub topic_prefix: String,
    pub qos: u8,
    pub tls: bool,
    pub username: String,
    pub password: SecretRefConfig,
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
    #[serde(with = "humantime_serde")]
    pub reconnect_delay: Duration,
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
                env: "AUTHGUARD__AUTHZ__SCOPE_DELIVERY__DIRECT_CONTEXT_HMAC_KEY".to_owned(),
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

fn default_component_instance_key() -> String {
    "default".to_owned()
}

impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            instance_key: default_component_instance_key(),
            provider: StorageProvider::default(),
            sqlite: SqliteStorageConfig::default(),
            postgres: PostgresStorageConfig::default(),
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

impl Default for WalletKeyRingConfig {
    fn default() -> Self {
        Self {
            active_key_id: "wallet-kek-v1".to_owned(),
            keys: BTreeMap::from([(
                "wallet-kek-v1".to_owned(),
                SecretRefConfig {
                    file: String::new(),
                    env: "AGENTVAULT_WALLET_KEK_V1".to_owned(),
                },
            )]),
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
                    env: "AGENTVAULT_RECOVERY_KEY_V1".to_owned(),
                },
            )]),
        }
    }
}

impl Default for PostgresStorageConfig {
    fn default() -> Self {
        Self {
            dsn: SecretRefConfig::default(),
            max_connections: 16,
            connect_timeout: Duration::from_secs(5),
        }
    }
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            instance_key: default_component_instance_key(),
            provider: CacheProvider::default(),
            memory: MemoryCacheConfig::default(),
            redis: RedisCacheConfig::default(),
        }
    }
}

impl Default for MemoryCacheConfig {
    fn default() -> Self {
        Self {
            max_entries: 10_000,
            max_value_bytes: 1024 * 1024,
            default_ttl: Duration::from_secs(5 * 60),
        }
    }
}

impl Default for RedisCacheConfig {
    fn default() -> Self {
        Self {
            mode: RedisCacheMode::Standalone,
            urls: Vec::new(),
            key_prefix: "agentvault".to_owned(),
            max_value_bytes: 1024 * 1024,
            connect_timeout: Duration::from_secs(3),
            operation_timeout: Duration::from_secs(2),
            default_ttl: Duration::from_secs(5 * 60),
            retry_attempts: 5,
            min_retry_wait: Duration::from_millis(100),
            max_retry_wait: Duration::from_secs(2),
        }
    }
}

impl Default for PayerX402Config {
    fn default() -> Self {
        Self {
            enabled: false,
            max_challenge_ttl: Duration::from_secs(5 * 60),
            assets: Vec::new(),
        }
    }
}

impl Default for PayeeX402Config {
    fn default() -> Self {
        Self {
            enabled: false,
            max_challenge_ttl: Duration::from_secs(5 * 60),
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
        }
    }
}

impl Default for PayerMppConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_challenge_ttl: Duration::from_secs(5 * 60),
            assets: Vec::new(),
        }
    }
}

impl Default for PayeeMppConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            max_challenge_ttl: Duration::from_secs(5 * 60),
            realm: String::new(),
            challenge_secret: SecretRefConfig::default(),
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
            request_ttl: Duration::from_secs(15 * 60),
            grant_ttl: Duration::from_secs(5 * 60),
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
                    env: "AGENTVAULT_APPROVAL_JWT_KEY_V1".to_owned(),
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
            max_backoff: Duration::from_secs(5 * 60),
        }
    }
}

impl Default for LocalTransportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            socket_path: "/run/agentvault/agentvault.sock".to_owned(),
            max_connections: 64,
            request_timeout: Duration::from_secs(5),
        }
    }
}

impl Default for GrpcTransportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: IpAddr::from([127, 0, 0, 1]),
            port: 50051,
            max_message_bytes: 2 * 1024 * 1024,
            request_timeout: Duration::from_secs(30),
        }
    }
}

impl Default for MqttTransportConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".to_owned(),
            port: 1883,
            client_id: "agentvault".to_owned(),
            consumer_group: "agentvault-pool".to_owned(),
            topic_prefix: DEFAULT_MQTT_TOPIC_PREFIX.to_owned(),
            qos: 1,
            tls: false,
            username: String::new(),
            password: SecretRefConfig::default(),
            request_timeout: Duration::from_secs(10),
            reconnect_delay: Duration::from_secs(1),
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
        ConfigLoader::builder()
            .add_source(File::from(path))
            .add_source(
                Environment::with_prefix("AGENTVAULT")
                    .prefix_separator("__")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()
            .and_then(ConfigLoader::try_deserialize::<Self>)
            .map_err(|error| Error::Config(error.to_string()))
    }

    /// Validates the complete daemon configuration.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe or internally inconsistent runtime configuration.
    pub fn validate(&self) -> Result<()> {
        self.validate_for_serve()
    }

    /// Validates all listeners, protocol limits, policies, and observability settings.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsafe or internally inconsistent runtime configuration.
    pub fn validate_for_serve(&self) -> Result<()> {
        self.validate_storage()?;
        self.validate_cache()?;
        self.validate_agentvault()?;
        if self.server.service_name.trim().is_empty() || self.server.shutdown_timeout.is_zero() {
            return Err(Error::Config(
                "server.service_name and server.shutdown_timeout must be set".to_owned(),
            ));
        }
        if !self.server.enabled
            && !self.mgmt.enabled
            && !self.agentvault.transport.grpc.enabled
            && !self.agentvault.transport.local.enabled
            && !self.agentvault.transport.mqtt.enabled
        {
            return Err(Error::Config(
                "at least one HTTP, management, gRPC, local, or MQTT component must be enabled".to_owned(),
            ));
        }
        if self.server.enabled {
            validate_path("server.context_path", &self.server.context_path)?;
            if self.server.port == 0 {
                return Err(Error::Config("enabled API server has an invalid port".to_owned()));
            }
            if !(256..=MAX_REQUEST_BYTES).contains(&self.server.request.max_body_bytes)
                || self.server.request.timeout.is_zero()
            {
                return Err(Error::Config("server.request limits are invalid".to_owned()));
            }
        }
        if (self.server.enabled || self.agentvault.transport.grpc.enabled) && self.server.authguard.enabled {
            validate_secret_ref(
                "server.authguard.direct_context_hmac_key",
                &self.server.authguard.direct_context_hmac_key,
            )?;
            let resolver = &self.server.authguard.scope_resolver;
            if resolver.enabled
                && (resolver.target.trim().is_empty()
                    || !(resolver.target.starts_with("http://") || resolver.target.starts_with("https://"))
                    || resolver.timeout.is_zero())
            {
                return Err(Error::Config(
                    "server.authguard.scope_resolver configuration is invalid".to_owned(),
                ));
            }
        }
        self.validate_management()?;
        if self.mgmt.enabled
            && self.server.enabled
            && self.mgmt.host == self.server.host
            && self.mgmt.port == self.server.port
        {
            return Err(Error::Config(
                "API and management listeners must use different addresses".to_owned(),
            ));
        }
        if self.agentvault.transport.grpc.enabled
            && ((self.server.enabled
                && self.agentvault.transport.grpc.host == self.server.host
                && self.agentvault.transport.grpc.port == self.server.port)
                || (self.mgmt.enabled
                    && self.agentvault.transport.grpc.host == self.mgmt.host
                    && self.agentvault.transport.grpc.port == self.mgmt.port))
        {
            return Err(Error::Config(
                "gRPC, HTTP API, and management listeners must use different addresses".to_owned(),
            ));
        }
        self.validate_transport()
    }

    /// Returns the configured absolute authoritative `SQLite` database path.
    #[must_use]
    pub fn sqlite_database_path(&self) -> std::path::PathBuf {
        std::path::PathBuf::from(&self.storage.sqlite.database_file)
    }

    fn validate_agentvault(&self) -> Result<()> {
        validate_wallet_key_ring(&self.agentvault.key_management)?;
        validate_recovery_encryption(&self.agentvault.recovery_encryption)?;
        self.validate_payer()?;
        self.validate_payee()?;
        self.validate_chains()?;
        self.validate_agentvault_runtime()
    }

    fn validate_storage(&self) -> Result<()> {
        validate_config_identifier("storage.instance_key", &self.storage.instance_key)?;
        match self.storage.provider {
            StorageProvider::Sqlite => {
                let sqlite = &self.storage.sqlite;
                if sqlite.database_file.trim().is_empty()
                    || sqlite.database_file == ":memory:"
                    || sqlite.database_file.contains('\0')
                    || !Path::new(&sqlite.database_file).is_absolute()
                    || sqlite.max_connections == 0
                    || sqlite.max_connections > MAX_STORAGE_CONNECTIONS
                    || sqlite.busy_timeout.is_zero()
                {
                    return Err(Error::Config("storage.sqlite configuration is invalid".to_owned()));
                }
            }
            StorageProvider::Postgres => {
                let postgres = &self.storage.postgres;
                validate_secret_ref("storage.postgres.dsn", &postgres.dsn)?;
                if postgres.max_connections == 0
                    || postgres.max_connections > MAX_STORAGE_CONNECTIONS
                    || postgres.connect_timeout.is_zero()
                {
                    return Err(Error::Config(
                        "storage.postgres connection limits are invalid".to_owned(),
                    ));
                }
            }
        }
        Ok(())
    }

    fn validate_cache(&self) -> Result<()> {
        validate_config_identifier("cache.instance_key", &self.cache.instance_key)?;
        match self.cache.provider {
            CacheProvider::Memory => {
                let memory = &self.cache.memory;
                if memory.max_entries == 0
                    || memory.max_entries > MAX_CACHE_ENTRIES
                    || memory.max_value_bytes == 0
                    || memory.max_value_bytes > MAX_CACHE_VALUE_BYTES
                    || memory.default_ttl.is_zero()
                    || memory.default_ttl > MAX_CACHE_TTL
                {
                    return Err(Error::Config("cache.memory configuration is invalid".to_owned()));
                }
            }
            CacheProvider::Redis => {
                let redis = &self.cache.redis;
                if redis.urls.is_empty()
                    || redis.urls.len() > MAX_CACHE_REDIS_ENDPOINTS
                    || (redis.mode == RedisCacheMode::Standalone && redis.urls.len() != 1)
                {
                    return Err(Error::Config(
                        "cache.redis URL count is invalid for its mode".to_owned(),
                    ));
                }
                for (index, url) in redis.urls.iter().enumerate() {
                    validate_secret_ref(&format!("cache.redis.urls.{index}"), url)?;
                    if !url.file.is_empty() || url.env.is_empty() {
                        return Err(Error::Config("cache.redis URLs must use SecretRef.env".to_owned()));
                    }
                }
                if redis.key_prefix.trim().is_empty()
                    || redis.key_prefix.len() > 128
                    || redis.key_prefix.starts_with(':')
                    || redis.key_prefix.ends_with(':')
                    || !redis
                        .key_prefix
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'.' | b'_' | b'-'))
                    || redis.max_value_bytes == 0
                    || redis.max_value_bytes > MAX_CACHE_VALUE_BYTES
                    || redis.connect_timeout.is_zero()
                    || redis.operation_timeout.is_zero()
                    || redis.default_ttl.is_zero()
                    || redis.default_ttl > MAX_CACHE_TTL
                    || redis.retry_attempts > MAX_CACHE_RETRIES
                    || redis.min_retry_wait.is_zero()
                    || redis.max_retry_wait < redis.min_retry_wait
                {
                    return Err(Error::Config("cache.redis configuration is invalid".to_owned()));
                }
            }
        }
        Ok(())
    }

    fn validate_payer(&self) -> Result<()> {
        let payer = &self.agentvault.payer;
        let mut enabled_adapters = 0_u8;
        validate_challenge_ttl("agentvault.payer.x402", payer.x402.max_challenge_ttl)?;
        if payer.x402.enabled {
            enabled_adapters = enabled_adapters.saturating_add(1);
            validate_x402_assets("agentvault.payer.x402.assets", &payer.x402.assets)?;
        }
        validate_challenge_ttl("agentvault.payer.mpp", payer.mpp.max_challenge_ttl)?;
        if payer.mpp.enabled {
            return Err(Error::Config(
                "agentvault.payer.mpp is unavailable until the Tempo payer signing plan is implemented".to_owned(),
            ));
        }
        validate_direction_enabled("payer", payer.enabled, enabled_adapters)
    }

    fn validate_payee(&self) -> Result<()> {
        let payee = &self.agentvault.payee;
        let mut enabled_adapters = 0_u8;
        validate_challenge_ttl("agentvault.payee.x402", payee.x402.max_challenge_ttl)?;
        if payee.x402.enabled {
            enabled_adapters = enabled_adapters.saturating_add(1);
            validate_x402_assets("agentvault.payee.x402.assets", &payee.x402.assets)?;
            validate_external_provider("agentvault.payee.x402.facilitator", &payee.x402.facilitator)?;
        }
        validate_challenge_ttl("agentvault.payee.mpp", payee.mpp.max_challenge_ttl)?;
        if payee.mpp.enabled {
            enabled_adapters = enabled_adapters.saturating_add(1);
            if payee.mpp.realm.trim().is_empty()
                || payee.mpp.realm.len() > 256
                || payee.mpp.realm.chars().any(char::is_control)
            {
                return Err(Error::Config("agentvault.payee.mpp.realm is invalid".to_owned()));
            }
            validate_secret_ref("agentvault.payee.mpp.challenge_secret", &payee.mpp.challenge_secret)?;
            validate_mpp_assets("agentvault.payee.mpp.assets", &payee.mpp.assets)?;
            validate_external_provider("agentvault.payee.mpp.provider", &payee.mpp.provider)?;
        }
        validate_direction_enabled("payee", payee.enabled, enabled_adapters)
    }

    fn validate_chains(&self) -> Result<()> {
        for (network, chain) in &self.agentvault.chains {
            validate_chain_id(network, chain.family)?;
            validate_operator_endpoint(
                &format!("agentvault.chains.{network}.rpc.endpoint"),
                &chain.rpc.endpoint,
                chain.rpc.allow_insecure_loopback,
            )?;
            if chain.rpc.timeout.is_zero()
                || chain.rpc.max_response_bytes == 0
                || chain.rpc.max_response_bytes > MAX_RPC_RESPONSE_BYTES
                || chain.confirmation_depth == 0
                || chain.confirmation_depth > 10_000
            {
                return Err(Error::Config(format!(
                    "agentvault.chains.{network} request or confirmation limits are invalid"
                )));
            }
            validate_secret_headers(&format!("agentvault.chains.{network}.rpc"), &chain.rpc.secret_headers)?;
        }
        for network in enabled_asset_networks(&self.agentvault) {
            let chain = self.agentvault.chains.get(network).ok_or_else(|| {
                Error::Config(format!(
                    "agentvault.chains has no fixed endpoint for enabled network {network}"
                ))
            })?;
            if chain.family != ChainFamily::Evm {
                return Err(Error::Config(format!(
                    "enabled x402/MPP Tempo network {network} must use an EVM chain"
                )));
            }
        }
        Ok(())
    }

    fn validate_agentvault_runtime(&self) -> Result<()> {
        if self.agentvault.approval.request_ttl.is_zero()
            || self.agentvault.approval.grant_ttl.is_zero()
            || self.agentvault.approval.grant_ttl > self.agentvault.approval.request_ttl
            || self.agentvault.approval.expiry_interval.is_zero()
            || self.agentvault.approval.expiry_batch_size == 0
            || self.agentvault.approval.expiry_batch_size > MAX_RECONCILIATION_BATCH_SIZE
        {
            return Err(Error::Config(
                "agentvault.approval TTL or expiration scheduling limits are invalid".to_owned(),
            ));
        }
        validate_approval_jwt(&self.agentvault.approval.jwt)?;

        let reconciliation = &self.agentvault.reconciliation;
        if reconciliation.interval.is_zero()
            || reconciliation.lease_ttl < reconciliation.interval
            || reconciliation.batch_size == 0
            || reconciliation.batch_size > MAX_RECONCILIATION_BATCH_SIZE
            || reconciliation.initial_backoff.is_zero()
            || reconciliation.max_backoff < reconciliation.initial_backoff
        {
            return Err(Error::Config(
                "agentvault.reconciliation scheduling limits are invalid".to_owned(),
            ));
        }
        if self.agentvault.enabled() && !reconciliation.enabled {
            return Err(Error::Config(
                "agentvault.reconciliation must be enabled when payer or payee is enabled".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_management(&self) -> Result<()> {
        if self.mgmt.enabled {
            if self.mgmt.port == 0 {
                return Err(Error::Config("mgmt.port must not be zero".to_owned()));
            }
            validate_path("mgmt.context_path", &self.mgmt.context_path)?;
            validate_path("mgmt.health.liveness_path", &self.mgmt.health.liveness_path)?;
            validate_path("mgmt.health.readiness_path", &self.mgmt.health.readiness_path)?;
            if self.mgmt.health.probe_timeout.is_zero() || self.mgmt.health.probe_timeout > MAX_HEALTH_PROBE_TIMEOUT {
                return Err(Error::Config(
                    "mgmt.health.probe_timeout must be between 1ns and 30s".to_owned(),
                ));
            }
            validate_path("mgmt.metrics.path", &self.mgmt.metrics.path)?;
            let mut routes = vec![
                self.mgmt.health.liveness_path.as_str(),
                self.mgmt.health.readiness_path.as_str(),
                "/debug/runtime",
            ];
            if self.mgmt.metrics.enabled {
                routes.push(self.mgmt.metrics.path.as_str());
            }
            routes.sort_unstable();
            if routes.windows(2).any(|pair| pair[0] == pair[1]) {
                return Err(Error::Config(
                    "management health, metrics, and runtime routes must be distinct".to_owned(),
                ));
            }
        }
        if self.mgmt.otel.enabled
            && (self.mgmt.otel.endpoint.trim().is_empty()
                || !self.mgmt.otel.protocol.eq_ignore_ascii_case("grpc")
                || self.mgmt.otel.timeout.is_zero()
                || !(0.0..=1.0).contains(&self.mgmt.otel.sample_rate))
        {
            return Err(Error::Config("mgmt.otel configuration is invalid".to_owned()));
        }
        Ok(())
    }

    fn validate_transport(&self) -> Result<()> {
        if self.agentvault.transport.grpc.enabled
            && (self.agentvault.transport.grpc.port == 0
                || !(256..=MAX_REQUEST_BYTES).contains(&self.agentvault.transport.grpc.max_message_bytes)
                || self.agentvault.transport.grpc.request_timeout.is_zero())
        {
            return Err(Error::Config(
                "enabled gRPC payment transport configuration is invalid".to_owned(),
            ));
        }
        if self.agentvault.transport.local.enabled
            && (self.agentvault.transport.local.socket_path.trim().is_empty()
                || self.agentvault.transport.local.socket_path.contains('\0')
                || !Path::new(&self.agentvault.transport.local.socket_path).is_absolute()
                || self.agentvault.transport.local.max_connections == 0
                || self.agentvault.transport.local.request_timeout.is_zero())
        {
            return Err(Error::Config(
                "enabled local payment transport configuration is invalid".to_owned(),
            ));
        }
        if self.agentvault.transport.mqtt.enabled {
            let mqtt = &self.agentvault.transport.mqtt;
            if mqtt.host.trim().is_empty()
                || mqtt.port == 0
                || validate_config_identifier("agentvault.transport.mqtt.client_id", &mqtt.client_id).is_err()
                || validate_config_identifier("agentvault.transport.mqtt.consumer_group", &mqtt.consumer_group).is_err()
                || !valid_topic_prefix(&mqtt.topic_prefix)
                || mqtt.qos != 1
                || mqtt.request_timeout.is_zero()
                || mqtt.reconnect_delay.is_zero()
            {
                return Err(Error::Config(
                    "enabled MQTT payment transport configuration is invalid".to_owned(),
                ));
            }
            validate_secret_ref("agentvault.transport.mqtt.password", &mqtt.password)?;
            if !mqtt.tls && !is_loopback_host(&mqtt.host) {
                return Err(Error::Config("remote MQTT payment transport requires TLS".to_owned()));
            }
        }
        Ok(())
    }
}

fn validate_wallet_key_ring(key_ring: &WalletKeyRingConfig) -> Result<()> {
    validate_config_identifier("agentvault.key_management.active_key_id", &key_ring.active_key_id)?;
    if key_ring.keys.is_empty() || key_ring.keys.len() > 16 || !key_ring.keys.contains_key(&key_ring.active_key_id) {
        return Err(Error::Config(
            "agentvault.key_management Wallet KEK ring is invalid".to_owned(),
        ));
    }
    for (key_id, reference) in &key_ring.keys {
        validate_config_identifier("agentvault.key_management key id", key_id)?;
        if !reference.file.is_empty() || reference.env.trim().is_empty() {
            return Err(Error::Config(format!(
                "agentvault.key_management.keys.{key_id} must use an environment reference"
            )));
        }
        validate_secret_ref(&format!("agentvault.key_management.keys.{key_id}"), reference)?;
    }
    Ok(())
}

fn validate_recovery_encryption(key_ring: &RecoveryEncryptionConfig) -> Result<()> {
    validate_config_identifier("agentvault.recovery_encryption.active_key_id", &key_ring.active_key_id)?;
    if key_ring.keys.is_empty() || key_ring.keys.len() > 16 || !key_ring.keys.contains_key(&key_ring.active_key_id) {
        return Err(Error::Config(
            "agentvault.recovery_encryption key ring is invalid".to_owned(),
        ));
    }
    for (key_id, reference) in &key_ring.keys {
        validate_config_identifier("agentvault.recovery_encryption key id", key_id)?;
        validate_secret_ref(&format!("agentvault.recovery_encryption.keys.{key_id}"), reference)?;
    }
    Ok(())
}

fn validate_challenge_ttl(field: &str, max_challenge_ttl: Duration) -> Result<()> {
    if max_challenge_ttl.is_zero() || max_challenge_ttl > MAX_CHALLENGE_TTL {
        return Err(Error::Config(format!("{field}.max_challenge_ttl is invalid")));
    }
    Ok(())
}

fn validate_direction_enabled(direction: &str, enabled: bool, enabled_adapters: u8) -> Result<()> {
    if enabled && enabled_adapters == 0 {
        return Err(Error::Config(format!(
            "agentvault.{direction}.enabled requires at least one enabled adapter"
        )));
    }
    if !enabled && enabled_adapters != 0 {
        return Err(Error::Config(format!(
            "agentvault.{direction} adapters must be disabled when the direction is disabled"
        )));
    }
    Ok(())
}

fn validate_x402_assets(field: &str, assets: &[X402AssetConfig]) -> Result<()> {
    if assets.is_empty() || assets.len() > MAX_PROTOCOL_ASSETS {
        return Err(Error::Config(format!(
            "{field} must contain a bounded trusted registry"
        )));
    }
    for (index, asset) in assets.iter().enumerate() {
        validate_evm_network(&format!("{field}.network"), &asset.network)?;
        if !valid_canonical_evm_address(&asset.contract)
            || asset.symbol != "USDC"
            || asset.decimals != 6
            || assets[..index]
                .iter()
                .any(|prior| prior.network == asset.network && prior.contract == asset.contract)
        {
            return Err(Error::Config(format!(
                "{field} contains an invalid or duplicate EVM USDC asset"
            )));
        }
    }
    Ok(())
}

fn validate_mpp_assets(field: &str, assets: &[MppAssetConfig]) -> Result<()> {
    if assets.is_empty() || assets.len() > MAX_PROTOCOL_ASSETS {
        return Err(Error::Config(format!(
            "{field} must contain a bounded trusted registry"
        )));
    }
    for (index, asset) in assets.iter().enumerate() {
        validate_evm_network(&format!("{field}.network"), &asset.network)?;
        if asset.currency.trim().is_empty()
            || asset.symbol.trim().is_empty()
            || asset
                .token_contract
                .as_deref()
                .is_some_and(|value| !valid_canonical_evm_address(value))
            || assets[..index]
                .iter()
                .any(|prior| prior.network == asset.network && prior.currency == asset.currency)
        {
            return Err(Error::Config(format!(
                "{field} contains an invalid or duplicate Tempo asset"
            )));
        }
    }
    Ok(())
}

fn enabled_asset_networks(config: &AgentVaultConfig) -> Vec<&str> {
    let mut networks = Vec::new();
    if config.payer.enabled && config.payer.x402.enabled {
        networks.extend(config.payer.x402.assets.iter().map(|asset| asset.network.as_str()));
    }
    if config.payer.enabled && config.payer.mpp.enabled {
        networks.extend(config.payer.mpp.assets.iter().map(|asset| asset.network.as_str()));
    }
    if config.payee.enabled && config.payee.x402.enabled {
        networks.extend(config.payee.x402.assets.iter().map(|asset| asset.network.as_str()));
    }
    if config.payee.enabled && config.payee.mpp.enabled {
        networks.extend(config.payee.mpp.assets.iter().map(|asset| asset.network.as_str()));
    }
    networks
}

fn validate_chain_id(network: &str, family: ChainFamily) -> Result<()> {
    match family {
        ChainFamily::Evm => validate_evm_network("agentvault.chains network", network),
        ChainFamily::Solana => {
            let valid = network.strip_prefix("solana:").is_some_and(|reference| {
                !reference.is_empty()
                    && reference.len() <= 128
                    && reference.bytes().all(|byte| byte.is_ascii_alphanumeric())
            });
            if valid {
                Ok(())
            } else {
                Err(Error::Config(
                    "agentvault.chains Solana network must be a canonical CAIP-2 identifier".to_owned(),
                ))
            }
        }
    }
}

fn validate_evm_network(field: &str, network: &str) -> Result<()> {
    if !network
        .strip_prefix("eip155:")
        .is_some_and(|chain| !chain.is_empty() && chain.bytes().all(|byte| byte.is_ascii_digit()))
    {
        return Err(Error::Config(format!("{field} must be a canonical EIP-155 network")));
    }
    Ok(())
}

fn valid_canonical_evm_address(value: &str) -> bool {
    value.len() == 42
        && value.starts_with("0x")
        && value == value.to_ascii_lowercase()
        && value[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn validate_operator_endpoint(field: &str, value: &str, allow_insecure_loopback: bool) -> Result<()> {
    let url = reqwest::Url::parse(value).map_err(|_| Error::Config(format!("{field} is invalid")))?;
    if url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.scheme(), "https" | "http")
        || (url.scheme() == "http" && (!allow_insecure_loopback || !url.host_str().is_some_and(is_loopback_host)))
    {
        return Err(Error::Config(format!("{field} is unsafe")));
    }
    Ok(())
}

fn validate_secret_headers(field: &str, headers: &BTreeMap<String, SecretHeaderConfig>) -> Result<()> {
    if headers.len() > 16 {
        return Err(Error::Config(format!("{field} has too many secret headers")));
    }
    for (header_name, header) in headers {
        let normalized = header_name.to_ascii_lowercase();
        if reqwest::header::HeaderName::from_bytes(header_name.as_bytes()).is_err()
            || matches!(
                normalized.as_str(),
                "connection"
                    | "content-length"
                    | "host"
                    | "idempotency-key"
                    | "payment-signature"
                    | "traceparent"
                    | "tracestate"
                    | "transfer-encoding"
            )
        {
            return Err(Error::Config(format!("{field} has an unsafe secret header name")));
        }
        validate_secret_ref(&format!("{field}.secret_headers.{header_name}.source"), &header.source)?;
        if header.prefix.len() > 64
            || !header
                .prefix
                .bytes()
                .all(|byte| byte == b' ' || byte.is_ascii_graphic())
        {
            return Err(Error::Config(format!("{field} has an unsafe secret header prefix")));
        }
    }
    Ok(())
}

fn validate_approval_jwt(jwt: &ApprovalJwtConfig) -> Result<()> {
    if jwt.issuer.trim().is_empty()
        || jwt.issuer.len() > 256
        || jwt.issuer.chars().any(char::is_control)
        || jwt.audience.trim().is_empty()
        || jwt.audience.len() > 256
        || jwt.audience.chars().any(char::is_control)
        || jwt.signing_keys.is_empty()
        || jwt.signing_keys.len() > 16
        || !jwt.signing_keys.contains_key(&jwt.active_key_id)
    {
        return Err(Error::Config(
            "agentvault.approval.jwt identity or key ring is invalid".to_owned(),
        ));
    }
    validate_config_identifier("agentvault.approval.jwt.active_key_id", &jwt.active_key_id)?;
    for (key_id, secret) in &jwt.signing_keys {
        validate_config_identifier("agentvault.approval.jwt.signing_keys key id", key_id)?;
        validate_secret_ref(&format!("agentvault.approval.jwt.signing_keys.{key_id}"), secret)?;
    }
    Ok(())
}

fn validate_path(field: &str, value: &str) -> Result<()> {
    if !value.starts_with('/') || (value.len() > 1 && value.ends_with('/')) || value.contains("//") {
        return Err(Error::Config(format!("{field} must be an absolute normalized path")));
    }
    Ok(())
}

fn validate_secret_ref(field: &str, secret: &SecretRefConfig) -> Result<()> {
    let has_file = !secret.file.trim().is_empty();
    let has_env = !secret.env.trim().is_empty();
    if has_file == has_env {
        return Err(Error::Config(format!("{field} must set exactly one of file or env")));
    }
    if has_file && (!Path::new(&secret.file).is_absolute() || secret.file.contains('\0')) {
        return Err(Error::Config(format!("{field}.file must be an absolute safe path")));
    }
    if has_env && !is_environment_name(&secret.env) {
        return Err(Error::Config(format!(
            "{field}.env is not a valid environment variable name"
        )));
    }
    Ok(())
}

fn is_environment_name(value: &str) -> bool {
    let mut bytes = value.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn validate_config_identifier(field: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':'))
    {
        return Err(Error::Config(format!("{field} is invalid")));
    }
    Ok(())
}

fn valid_topic_prefix(value: &str) -> bool {
    !value.trim().is_empty()
        && value.len() <= 256
        && !value.starts_with('/')
        && !value.ends_with('/')
        && !value.contains(['+', '#', '\0'])
        && value.split('/').all(|segment| !segment.is_empty())
}

fn validate_external_provider(field: &str, provider: &ExternalProviderConfig) -> Result<()> {
    validate_config_identifier(&format!("{field}.id"), &provider.id)?;
    let url = reqwest::Url::parse(&provider.base_url)
        .map_err(|error| Error::Config(format!("{field}.base_url is invalid: {error}")))?;
    if url.query().is_some()
        || url.fragment().is_some()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.host_str().is_none()
    {
        return Err(Error::Config(format!(
            "{field}.base_url must not contain credentials, query, or fragment"
        )));
    }
    match url.scheme() {
        "https" => {}
        "http" if provider.allow_insecure_loopback && url.host_str().is_some_and(is_loopback_host) => {}
        _ => {
            return Err(Error::Config(format!(
                "{field} must use HTTPS; HTTP is limited to explicit loopback development"
            )));
        }
    }
    validate_provider_operation_path(field, "verify_path", &provider.verify_path)?;
    validate_provider_operation_path(field, "settle_path", &provider.settle_path)?;
    if provider.timeout.is_zero()
        || provider.max_response_bytes == 0
        || provider.max_response_bytes > MAX_PROVIDER_RESPONSE_BYTES
        || provider.secret_headers.len() > 16
    {
        return Err(Error::Config(format!("{field} request limits are invalid")));
    }
    validate_secret_headers(field, &provider.secret_headers)?;
    Ok(())
}

fn validate_provider_operation_path(provider_field: &str, field: &str, value: &str) -> Result<()> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains(['?', '#'])
        || value
            .split('/')
            .any(|part| part.is_empty() || part == ".." || part == ".")
    {
        return Err(Error::Config(format!("{provider_field}.{field} is unsafe")));
    }
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || host.parse::<IpAddr>().is_ok_and(|address| address.is_loopback())
}

#[cfg(test)]
#[path = "../../tests/config/config_test.rs"]
mod tests;
