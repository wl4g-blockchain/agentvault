use super::{Deserialize, Duration, IpAddr, Serialize};

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

#[cfg(test)]
#[path = "../../tests/config/foundation_test.rs"]
mod tests;
