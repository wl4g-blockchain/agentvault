//! Strongly typed process configuration.
//!
//! Precedence is: built-in defaults < YAML file < `AGENTVAULT__...`
//! environment variables. Double underscores address nested fields, for
//! example `AGENTVAULT__MGMT__OTEL__ENABLED=true`.

use std::{collections::HashSet, net::IpAddr, path::Path, time::Duration};

use ::config::{Config as ConfigLoader, Environment, File};
use serde::{Deserialize, Serialize};

use crate::{model::sign::ClientPolicy, Error, Result};

pub const DEFAULT_REQUEST_TOPIC_PREFIX: &str = "wallet/v1/sign/requests";
pub const DEFAULT_RESPONSE_TOPIC_PREFIX: &str = "wallet/v1/sign/responses";
const MAX_CLIENT_POLICIES: usize = 1_024;
const MAX_POLICY_VALUES: usize = 256;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_TTL_SECONDS: i64 = 300;
const MAX_CLOCK_SKEW_SECONDS: i64 = 60;
const MAX_DEDUP_TTL_SECONDS: i64 = 86_400;
const MAX_DEDUP_ENTRIES: usize = 1_000_000;
const MAX_HISTORY_ENTRIES: usize = 1_000_000;

/// Complete `AgentVault` configuration. Definitions intentionally follow the
/// same outer-to-inner order as the YAML document.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppConfig {
    pub server: ServerConfig,
    pub mgmt: ManagementConfig,
    pub logging: LoggingConfig,
    pub store: StoreConfig,
    pub security: SecurityConfig,
    pub signers: SignersConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ServerConfig {
    pub enabled: bool,
    pub service_name: String,
    pub host: IpAddr,
    pub port: u16,
    pub context_path: String,
    pub api_token_file: String,
    #[serde(with = "humantime_serde")]
    pub shutdown_timeout: Duration,
    pub request: RequestConfig,
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

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct StoreConfig {
    pub directory: String,
    pub master_key_file: String,
    pub sign_history_file: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    pub client_policies: Vec<ClientPolicy>,
    pub max_request_bytes: usize,
    pub max_request_ttl_seconds: i64,
    pub clock_skew_seconds: i64,
    pub dedup_ttl_seconds: i64,
    pub max_dedup_entries: usize,
    pub max_history_entries: usize,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct SignersConfig {
    pub local: LocalConfig,
    pub mqtt: MqttConfig,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalConfig {
    pub enabled: bool,
    pub socket_path: String,
    pub max_connections: usize,
    #[serde(with = "humantime_serde")]
    pub request_timeout: Duration,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct MqttConfig {
    pub enabled: bool,
    pub host: String,
    pub port: u16,
    pub client_id: String,
    pub consumer_group: String,
    pub request_topic_prefix: String,
    pub response_topic_prefix: String,
    pub qos: u8,
    pub tls: bool,
    pub username: String,
    pub password_file: String,
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
            api_token_file: String::new(),
            shutdown_timeout: Duration::from_secs(15),
            request: RequestConfig::default(),
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

impl Default for StoreConfig {
    fn default() -> Self {
        Self {
            directory: "/var/lib/agentvault".to_owned(),
            master_key_file: "/run/secrets/agentvault/master.key".to_owned(),
            sign_history_file: "sign-history.jsonl".to_owned(),
        }
    }
}

impl Default for SecurityConfig {
    fn default() -> Self {
        Self {
            client_policies: Vec::new(),
            max_request_bytes: 16 * 1024,
            max_request_ttl_seconds: 30,
            clock_skew_seconds: 5,
            dedup_ttl_seconds: 300,
            max_dedup_entries: 10_000,
            max_history_entries: 10_000,
        }
    }
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            socket_path: "/run/agentvault/agentvault.sock".to_owned(),
            max_connections: 64,
            request_timeout: Duration::from_secs(5),
        }
    }
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".to_owned(),
            port: 1883,
            client_id: "agentvault".to_owned(),
            consumer_group: "agentvault-pool".to_owned(),
            request_topic_prefix: DEFAULT_REQUEST_TOPIC_PREFIX.to_owned(),
            response_topic_prefix: DEFAULT_RESPONSE_TOPIC_PREFIX.to_owned(),
            qos: 1,
            tls: false,
            username: String::new(),
            password_file: String::new(),
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
    /// Returns an error when the source cannot be loaded or store paths are invalid.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let config = ConfigLoader::builder()
            .add_source(File::from(path))
            .add_source(
                Environment::with_prefix("AGENTVAULT")
                    .prefix_separator("__")
                    .separator("__")
                    .try_parsing(true),
            )
            .build()
            .and_then(ConfigLoader::try_deserialize::<Self>)
            .map_err(|error| Error::Config(error.to_string()))?;
        config.validate_store()?;
        Ok(config)
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
        self.validate_store()?;
        if self.server.service_name.trim().is_empty() || self.server.shutdown_timeout.is_zero() {
            return Err(Error::Config(
                "server.service_name and server.shutdown_timeout must be set".to_owned(),
            ));
        }
        if !self.server.enabled && !self.signers.local.enabled && !self.signers.mqtt.enabled {
            return Err(Error::Config(
                "at least one API, local, or MQTT signing endpoint must be enabled".to_owned(),
            ));
        }
        if self.server.enabled {
            validate_path("server.context_path", &self.server.context_path)?;
            if self.server.port == 0 {
                return Err(Error::Config("enabled API server has an invalid port".to_owned()));
            }
            if !self.server.host.is_loopback() && self.server.api_token_file.trim().is_empty() {
                return Err(Error::Config(
                    "server.api_token_file is required when the API binds a non-loopback address".to_owned(),
                ));
            }
            if !(256..=MAX_REQUEST_BYTES).contains(&self.server.request.max_body_bytes)
                || self.server.request.timeout.is_zero()
            {
                return Err(Error::Config("server.request limits are invalid".to_owned()));
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
        self.validate_security()?;
        self.validate_signers()
    }

    #[must_use]
    pub fn history_path(&self) -> std::path::PathBuf {
        let configured = std::path::Path::new(&self.store.sign_history_file);
        if configured.is_absolute() {
            configured.to_path_buf()
        } else {
            std::path::Path::new(&self.store.directory).join(configured)
        }
    }

    fn validate_store(&self) -> Result<()> {
        if self.store.directory.trim().is_empty()
            || self.store.master_key_file.trim().is_empty()
            || self.store.sign_history_file.trim().is_empty()
        {
            return Err(Error::Config("store paths must not be empty".to_owned()));
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
            validate_path("mgmt.metrics.path", &self.mgmt.metrics.path)?;
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

    fn validate_security(&self) -> Result<()> {
        if self.security.client_policies.is_empty() {
            return Err(Error::Config("security.client_policies must not be empty".to_owned()));
        }
        if self.security.client_policies.len() > MAX_CLIENT_POLICIES {
            return Err(Error::Config(format!(
                "security.client_policies must not exceed {MAX_CLIENT_POLICIES} entries"
            )));
        }
        let mut prefixes = HashSet::new();
        for policy in &self.security.client_policies {
            if crate::model::sign::validate_identifier("client_id_prefix", &policy.client_id_prefix).is_err()
                || policy.wallet_ids.is_empty()
                || policy.purposes.is_empty()
                || policy.wallet_ids.len() > MAX_POLICY_VALUES
                || policy.purposes.len() > MAX_POLICY_VALUES
                || policy
                    .wallet_ids
                    .iter()
                    .any(|id| crate::model::sign::validate_identifier("wallet_id", id).is_err())
                || policy
                    .purposes
                    .iter()
                    .any(|purpose| crate::model::sign::validate_identifier("purpose", purpose).is_err())
            {
                return Err(Error::Config(
                    "each client policy requires a safe prefix and non-empty wallet_ids and purposes".to_owned(),
                ));
            }
            if !prefixes.insert(policy.client_id_prefix.as_str()) {
                return Err(Error::Config(
                    "security.client_policies must use unique client_id_prefix values".to_owned(),
                ));
            }
        }
        if !(256..=MAX_REQUEST_BYTES).contains(&self.security.max_request_bytes) {
            return Err(Error::Config(format!(
                "security.max_request_bytes must be between 256 and {MAX_REQUEST_BYTES}"
            )));
        }
        if self.security.max_request_ttl_seconds <= 0
            || self.security.max_request_ttl_seconds > MAX_REQUEST_TTL_SECONDS
            || self.security.clock_skew_seconds < 0
            || self.security.clock_skew_seconds > MAX_CLOCK_SKEW_SECONDS
            || self.security.dedup_ttl_seconds <= 0
            || self.security.dedup_ttl_seconds > MAX_DEDUP_TTL_SECONDS
            || self.security.max_dedup_entries == 0
            || self.security.max_dedup_entries > MAX_DEDUP_ENTRIES
            || self.security.max_history_entries == 0
            || self.security.max_history_entries > MAX_HISTORY_ENTRIES
        {
            return Err(Error::Config(
                "request, dedup, or history limits are invalid".to_owned(),
            ));
        }
        let accepted_lifetime = self
            .security
            .max_request_ttl_seconds
            .checked_add(self.security.clock_skew_seconds)
            .ok_or_else(|| Error::Config("security request lifetime overflows".to_owned()))?;
        if self.security.dedup_ttl_seconds < accepted_lifetime {
            return Err(Error::Config(
                "security.dedup_ttl_seconds must cover the accepted request lifetime".to_owned(),
            ));
        }
        Ok(())
    }

    fn validate_signers(&self) -> Result<()> {
        if self.signers.local.enabled
            && (self.signers.local.socket_path.trim().is_empty()
                || self.signers.local.max_connections == 0
                || self.signers.local.request_timeout.is_zero())
        {
            return Err(Error::Config(
                "enabled local signer configuration is invalid".to_owned(),
            ));
        }
        if self.signers.mqtt.enabled {
            let mqtt = &self.signers.mqtt;
            if mqtt.host.trim().is_empty()
                || mqtt.port == 0
                || crate::model::sign::validate_identifier("mqtt.client_id", &mqtt.client_id).is_err()
                || crate::model::sign::validate_identifier("mqtt.consumer_group", &mqtt.consumer_group).is_err()
                || mqtt.request_topic_prefix.trim().is_empty()
                || mqtt.response_topic_prefix.trim().is_empty()
                || mqtt.request_topic_prefix.contains(['+', '#'])
                || mqtt.response_topic_prefix.contains(['+', '#'])
                || mqtt.qos > 2
                || mqtt.request_timeout.is_zero()
                || mqtt.reconnect_delay.is_zero()
            {
                return Err(Error::Config("enabled MQTT signer configuration is invalid".to_owned()));
            }
        }
        Ok(())
    }
}

fn validate_path(field: &str, value: &str) -> Result<()> {
    if !value.starts_with('/') || (value.len() > 1 && value.ends_with('/')) || value.contains("//") {
        return Err(Error::Config(format!("{field} must be an absolute normalized path")));
    }
    Ok(())
}

/// Backwards-compatible short name for consumers of the original crate API.
pub type Config = AppConfig;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_config_without_a_data_plane() {
        let mut config = AppConfig::default();
        config.server.enabled = false;
        config.security.client_policies.push(policy());
        assert!(config.validate_for_serve().is_err());
    }

    #[test]
    fn requires_authentication_for_non_loopback_api() {
        let mut config = AppConfig::default();
        config.server.host = IpAddr::from([0, 0, 0, 0]);
        config.security.client_policies.push(policy());
        assert!(
            matches!(config.validate_for_serve(), Err(Error::Config(message)) if message.contains("api_token_file"))
        );
    }

    #[test]
    fn resolves_relative_history_below_the_store() {
        let mut config = AppConfig::default();
        config.store.directory = "/tmp/agentvault".to_owned();
        assert_eq!(
            config.history_path(),
            std::path::PathBuf::from("/tmp/agentvault/sign-history.jsonl")
        );
    }

    #[test]
    fn canonical_yaml_is_valid() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("etc/agentvault.yaml");
        AppConfig::load(path).unwrap().validate_for_serve().unwrap();
    }

    #[test]
    fn validates_otel_even_when_management_listener_is_disabled() {
        let mut config = AppConfig::default();
        config.security.client_policies.push(policy());
        config.mgmt.enabled = false;
        config.mgmt.otel.enabled = true;
        config.mgmt.otel.protocol = "http".to_owned();
        assert!(matches!(config.validate_for_serve(), Err(Error::Config(message)) if message.contains("mgmt.otel")));
    }

    fn policy() -> ClientPolicy {
        ClientPolicy {
            client_id_prefix: "flowgent-".to_owned(),
            wallet_ids: vec!["payer".to_owned()],
            purposes: vec!["x402.payment".to_owned()],
        }
    }
}
