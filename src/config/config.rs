//! Strongly typed process configuration.
//!
//! Precedence is: built-in defaults < YAML file < `AGENTVAULT__...`
//! environment variables. Double underscores address nested fields, for
//! example `AGENTVAULT__MGMT__OTEL__ENABLED=true`.
//!
//! Sensitive leaves use the same canonical field-path names. Their values are
//! resolved by [`SecretRefConfig`] and are deliberately filtered out of the
//! serializable configuration source.

use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    net::IpAddr,
    path::Path,
    time::Duration,
};

use ::config::{Config as ConfigLoader, Environment, File};
use serde::{Deserialize, Serialize};
use solana_pubkey::Pubkey;
use x402_chain_solana::chain::SolanaChainReference;
use x402_types::chain::ChainId;

use crate::{
    model::{wallet::WalletSelectionStrategy, Amount},
    Error, Result,
};

pub const DEFAULT_MQTT_TOPIC_PREFIX: &str = "agentvault/v1/clients";
pub(crate) const SECRETS_ENV_FILE_ENV: &str = "AGENTVAULT__SECRETS__ENV_FILE";
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_GRPC_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_CACHE_ENTRIES: usize = 1_000_000;
pub(crate) const MAX_CACHE_VALUE_BYTES: usize = 16 * 1024 * 1024;
pub(crate) const MAX_CACHE_REDIS_ENDPOINTS: usize = 32;
pub(crate) const MAX_CACHE_RETRIES: u32 = 32;
pub(crate) const MAX_CACHE_TTL: Duration = Duration::from_hours(24);
const MAX_STORAGE_CONNECTIONS: u32 = 256;
const MAX_PROVIDER_RESPONSE_BYTES: usize = 4 * 1024 * 1024;
const MAX_RECONCILIATION_BATCH_SIZE: usize = 10_000;
const MAX_CHALLENGE_TTL: Duration = Duration::from_hours(24);
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

impl AppConfig {
    /// Loads YAML and applies `AGENTVAULT__...` overrides.
    ///
    /// # Errors
    ///
    /// Returns an error when the source cannot be loaded or deserialized.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let base = Self::load_file(path)?;
        let secret_variables = base.secret_environment_variables();
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
        Self::load_with_filtered_environment(path, source)
    }

    fn environment(source: HashMap<String, String>) -> Environment {
        Environment::with_prefix("AGENTVAULT")
            .prefix_separator("__")
            .separator("__")
            .try_parsing(true)
            .source(Some(source))
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
        let secret_variables = Self::load_file(path)?.secret_environment_variables();
        let source = source
            .into_iter()
            .filter(|(key, _)| !secret_variables.contains(key) && key != SECRETS_ENV_FILE_ENV)
            .collect();
        Self::load_with_filtered_environment(path, source)
    }

    fn load_with_filtered_environment(path: &Path, source: HashMap<String, String>) -> Result<Self> {
        ConfigLoader::builder()
            .add_source(File::from(path))
            .add_source(Self::environment(source))
            .build()
            .and_then(ConfigLoader::try_deserialize::<Self>)
            .map_err(|error| Error::Config(error.to_string()))
    }

    fn secret_environment_variables(&self) -> BTreeSet<String> {
        let mut names = BTreeSet::new();
        Self::insert(&mut names, &self.server.authguard.direct_context_hmac_key);
        Self::insert(&mut names, &self.storage.postgres.url);
        for reference in &self.cache.redis.urls {
            Self::insert(&mut names, reference);
        }
        Self::insert_headers(&mut names, &self.agentvault.payee.x402.facilitator.secret_headers);
        Self::insert(&mut names, &self.agentvault.payee.mpp.challenge_secret);
        Self::insert_headers(&mut names, &self.agentvault.payee.mpp.provider.secret_headers);
        for chain in self.agentvault.chains.values() {
            Self::insert_headers(&mut names, &chain.rpc.secret_headers);
        }
        for reference in self.agentvault.key_management.keys.values() {
            Self::insert(&mut names, reference);
        }
        for reference in self.agentvault.recovery_encryption.keys.values() {
            Self::insert(&mut names, reference);
        }
        for reference in self.agentvault.approval.jwt.signing_keys.values() {
            Self::insert(&mut names, reference);
        }
        Self::insert(&mut names, &self.agentvault.transport.mqtt.password);
        names
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
            && !self.agentvault.transport.mqtt.enabled
        {
            return Err(Error::Config(
                "at least one HTTP, management, gRPC, or MQTT component must be enabled".to_owned(),
            ));
        }
        if self.server.enabled {
            ConfigValidation::validate_path("server.context_path", &self.server.context_path)?;
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
            ConfigValidation::validate_secret_ref(
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
            && ConfigValidation::listener_addresses_overlap(
                self.mgmt.host,
                self.mgmt.port,
                self.server.host,
                self.server.port,
            )
        {
            return Err(Error::Config(
                "API and management listeners must use different addresses".to_owned(),
            ));
        }
        if self.agentvault.transport.grpc.enabled
            && ((self.server.enabled
                && ConfigValidation::listener_addresses_overlap(
                    self.agentvault.transport.grpc.host,
                    self.agentvault.transport.grpc.port,
                    self.server.host,
                    self.server.port,
                ))
                || (self.mgmt.enabled
                    && ConfigValidation::listener_addresses_overlap(
                        self.agentvault.transport.grpc.host,
                        self.agentvault.transport.grpc.port,
                        self.mgmt.host,
                        self.mgmt.port,
                    )))
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
        ConfigValidation::validate_wallet_key_ring(&self.agentvault.key_management)?;
        let cache_ttl = match self.cache.provider {
            CacheProvider::Memory => self.cache.memory.default_ttl,
            CacheProvider::Redis => self.cache.redis.default_ttl,
        };
        if self.agentvault.key_management.import_session_ttl < Duration::from_secs(30)
            || self.agentvault.key_management.import_session_ttl > Duration::from_mins(5)
            || self.agentvault.key_management.import_session_ttl > cache_ttl
        {
            return Err(Error::Config(
                "agentvault.key_management.import_session_ttl must be between 30s and 5m and not exceed the selected cache default_ttl"
                    .to_owned(),
            ));
        }
        ConfigValidation::validate_recovery_encryption(&self.agentvault.recovery_encryption)?;
        self.validate_payer()?;
        self.validate_payee()?;
        self.validate_chains()?;
        self.validate_agentvault_runtime()
    }

    fn validate_storage(&self) -> Result<()> {
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
                ConfigValidation::validate_secret_ref("storage.postgres.url", &postgres.url)?;
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
                    ConfigValidation::validate_secret_ref(&format!("cache.redis.urls.{index}"), url)?;
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
        ConfigValidation::validate_challenge_ttl("agentvault.payer.x402", payer.x402.max_challenge_ttl)?;
        if payer.x402.enabled {
            enabled_adapters = enabled_adapters.saturating_add(1);
            ConfigValidation::validate_x402_assets("agentvault.payer.x402.assets", &payer.x402.assets)?;
        }
        ConfigValidation::validate_challenge_ttl("agentvault.payer.mpp", payer.mpp.max_challenge_ttl)?;
        if payer.mpp.enabled {
            enabled_adapters = enabled_adapters.saturating_add(1);
            ConfigValidation::validate_mpp_assets("agentvault.payer.mpp.assets", &payer.mpp.assets)?;
        }
        ConfigValidation::validate_direction_enabled("payer", payer.enabled, enabled_adapters)
    }

    fn validate_payee(&self) -> Result<()> {
        let payee = &self.agentvault.payee;
        let mut enabled_adapters = 0_u8;
        ConfigValidation::validate_challenge_ttl("agentvault.payee.x402", payee.x402.max_challenge_ttl)?;
        if payee.x402.enabled {
            enabled_adapters = enabled_adapters.saturating_add(1);
            ConfigValidation::validate_x402_assets("agentvault.payee.x402.assets", &payee.x402.assets)?;
            ConfigValidation::validate_external_provider("agentvault.payee.x402.facilitator", &payee.x402.facilitator)?;
        }
        ConfigValidation::validate_challenge_ttl("agentvault.payee.mpp", payee.mpp.max_challenge_ttl)?;
        if payee.mpp.enabled {
            enabled_adapters = enabled_adapters.saturating_add(1);
            if payee.mpp.realm.trim().is_empty()
                || payee.mpp.realm.len() > 256
                || payee.mpp.realm.chars().any(char::is_control)
            {
                return Err(Error::Config("agentvault.payee.mpp.realm is invalid".to_owned()));
            }
            ConfigValidation::validate_secret_ref(
                "agentvault.payee.mpp.challenge_secret",
                &payee.mpp.challenge_secret,
            )?;
            ConfigValidation::validate_mpp_assets("agentvault.payee.mpp.assets", &payee.mpp.assets)?;
            ConfigValidation::validate_external_provider("agentvault.payee.mpp.provider", &payee.mpp.provider)?;
        }
        ConfigValidation::validate_direction_enabled("payee", payee.enabled, enabled_adapters)
    }

    fn validate_chains(&self) -> Result<()> {
        for asset in ConfigValidation::enabled_x402_assets(&self.agentvault) {
            let chain = self.agentvault.chains.get(&asset.network).ok_or_else(|| {
                Error::Config(format!(
                    "agentvault.chains has no fixed endpoint for enabled network {}",
                    asset.network
                ))
            })?;
            let expected = if ConfigValidation::is_evm_network(&asset.network) {
                ChainFamily::Evm
            } else {
                ChainFamily::Solana
            };
            if chain.family != expected {
                return Err(Error::Config(format!(
                    "x402 network {} does not match its configured chain family",
                    asset.network
                )));
            }
        }
        for network in ConfigValidation::enabled_mpp_networks(&self.agentvault) {
            let chain = self.agentvault.chains.get(network).ok_or_else(|| {
                Error::Config(format!(
                    "agentvault.chains has no fixed endpoint for enabled network {network}"
                ))
            })?;
            if chain.family != ChainFamily::Evm {
                return Err(Error::Config(format!(
                    "enabled MPP Tempo network {network} must use an EVM chain"
                )));
            }
        }
        for (network, chain) in &self.agentvault.chains {
            ConfigValidation::validate_chain_id(network, chain.family)?;
            ConfigValidation::validate_operator_endpoint(
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
            ConfigValidation::validate_secret_headers(
                &format!("agentvault.chains.{network}.rpc"),
                &chain.rpc.secret_headers,
            )?;
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
        ConfigValidation::validate_approval_jwt(&self.agentvault.approval.jwt)?;

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
        let balance_sync = &self.agentvault.wallet_balance_sync;
        if balance_sync.interval.is_zero()
            || balance_sync.batch_size == 0
            || balance_sync.batch_size > MAX_RECONCILIATION_BATCH_SIZE
        {
            return Err(Error::Config(
                "agentvault.wallet_balance_sync scheduling limits are invalid".to_owned(),
            ));
        }
        if self.agentvault.payer.enabled && !balance_sync.enabled {
            return Err(Error::Config(
                "agentvault.wallet_balance_sync must be enabled when payer is enabled".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_management(&self) -> Result<()> {
        if self.mgmt.enabled {
            if self.mgmt.port == 0 {
                return Err(Error::Config("mgmt.port must not be zero".to_owned()));
            }
            ConfigValidation::validate_path("mgmt.context_path", &self.mgmt.context_path)?;
            ConfigValidation::validate_path("mgmt.health.liveness_path", &self.mgmt.health.liveness_path)?;
            ConfigValidation::validate_path("mgmt.health.readiness_path", &self.mgmt.health.readiness_path)?;
            if self.mgmt.health.probe_timeout.is_zero() || self.mgmt.health.probe_timeout > MAX_HEALTH_PROBE_TIMEOUT {
                return Err(Error::Config(
                    "mgmt.health.probe_timeout must be between 1ns and 30s".to_owned(),
                ));
            }
            ConfigValidation::validate_path("mgmt.metrics.path", &self.mgmt.metrics.path)?;
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
                || !(256..=MAX_GRPC_MESSAGE_BYTES).contains(&self.agentvault.transport.grpc.max_message_bytes)
                || self.agentvault.transport.grpc.request_timeout.is_zero())
        {
            return Err(Error::Config(
                "enabled gRPC payment transport configuration is invalid".to_owned(),
            ));
        }
        if self.agentvault.transport.mqtt.enabled {
            let mqtt = &self.agentvault.transport.mqtt;
            if mqtt.host.trim().is_empty()
                || mqtt.port == 0
                || ConfigValidation::validate_config_identifier("agentvault.transport.mqtt.client_id", &mqtt.client_id)
                    .is_err()
                || ConfigValidation::validate_config_identifier(
                    "agentvault.transport.mqtt.consumer_group",
                    &mqtt.consumer_group,
                )
                .is_err()
                || !ConfigValidation::valid_topic_prefix(&mqtt.topic_prefix)
                || mqtt.qos != 1
                || mqtt.request_timeout.is_zero()
                || mqtt.reconnect_delay.is_zero()
            {
                return Err(Error::Config(
                    "enabled MQTT payment transport configuration is invalid".to_owned(),
                ));
            }
            ConfigValidation::validate_secret_ref("agentvault.transport.mqtt.password", &mqtt.password)?;
            if !mqtt.tls && !ConfigValidation::is_loopback_host(&mqtt.host) {
                return Err(Error::Config("remote MQTT payment transport requires TLS".to_owned()));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/config/config_test.rs"]
mod tests;

impl AppConfig {
    fn insert(names: &mut BTreeSet<String>, reference: &SecretRefConfig) {
        if !reference.env.is_empty() {
            names.insert(reference.env.clone());
        }
    }
    fn insert_headers(names: &mut BTreeSet<String>, headers: &BTreeMap<String, SecretHeaderConfig>) {
        for header in headers.values() {
            Self::insert(names, &header.source);
        }
    }
}

#[path = "foundation.rs"]
mod foundation;
pub use foundation::*;
#[path = "payment.rs"]
mod payment;
pub use payment::*;
#[path = "validation.rs"]
mod validation;
use validation::ConfigValidation;
