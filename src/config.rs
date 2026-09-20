use std::{collections::HashSet, fs, path::Path};

use serde::Deserialize;

use crate::{
    protocol::{validate_identifier, ClientPolicy},
    Error, Result,
};

pub const DEFAULT_REQUEST_TOPIC_PREFIX: &str = "wallet/v1/sign/requests";
pub const DEFAULT_RESPONSE_TOPIC_PREFIX: &str = "wallet/v1/sign/responses";
const MAX_CLIENT_POLICIES: usize = 1_024;
const MAX_POLICY_VALUES: usize = 256;
const MAX_REQUEST_BYTES: usize = 1024 * 1024;
const MAX_REQUEST_TTL_SECONDS: i64 = 300;
const MAX_CLOCK_SKEW_SECONDS: i64 = 60;
const MAX_DEDUP_TTL_SECONDS: i64 = 86_400;
const MAX_DEDUP_ENTRIES: usize = 1_000_000;

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub store: StoreConfig,
    #[serde(default)]
    pub security: SecurityConfig,
    #[serde(default)]
    pub transports: TransportsConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoreConfig {
    pub directory: String,
    pub master_key_file: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct SecurityConfig {
    pub client_policies: Vec<ClientPolicy>,
    pub max_request_bytes: usize,
    pub max_request_ttl_seconds: i64,
    pub clock_skew_seconds: i64,
    pub dedup_ttl_seconds: i64,
    pub max_dedup_entries: usize,
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
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TransportsConfig {
    #[serde(default)]
    pub local: LocalConfig,
    #[serde(default)]
    pub mqtt: MqttConfig,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LocalConfig {
    pub enabled: bool,
    pub socket_path: String,
}

impl Default for LocalConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            socket_path: "/run/wallet/wallet.sock".to_owned(),
        }
    }
}

#[derive(Clone, Debug, Deserialize)]
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
}

impl Default for MqttConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            host: "127.0.0.1".to_owned(),
            port: 1883,
            client_id: "wallet".to_owned(),
            consumer_group: "wallet-pool".to_owned(),
            request_topic_prefix: DEFAULT_REQUEST_TOPIC_PREFIX.to_owned(),
            response_topic_prefix: DEFAULT_RESPONSE_TOPIC_PREFIX.to_owned(),
            qos: 1,
            tls: false,
            username: String::new(),
            password_file: String::new(),
        }
    }
}

impl Config {
    /// Loads configuration and validates the store fields needed by every command.
    ///
    /// # Errors
    ///
    /// Returns an error when the file cannot be read, TOML is invalid, or store
    /// paths are empty.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let source = fs::read_to_string(path).map_err(|source| Error::ReadFile {
            path: path.to_path_buf(),
            source,
        })?;
        let config: Self = toml::from_str(&source).map_err(|error| Error::Config(error.to_string()))?;
        config.validate_store()?;
        Ok(config)
    }

    /// Validates the complete daemon configuration.
    ///
    /// # Errors
    ///
    /// Returns an error when a transport, policy, or safety limit is invalid.
    pub fn validate(&self) -> Result<()> {
        self.validate_for_serve()
    }

    /// Validates fields required to serve signing requests.
    ///
    /// # Errors
    ///
    /// Returns an error when a transport, policy, or safety limit is invalid.
    pub fn validate_for_serve(&self) -> Result<()> {
        self.validate_store()?;
        if !self.transports.local.enabled && !self.transports.mqtt.enabled {
            return Err(Error::Config("at least one transport must be enabled".to_owned()));
        }
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
            if validate_identifier("client_id_prefix", &policy.client_id_prefix).is_err()
                || policy.wallet_ids.is_empty()
                || policy.purposes.is_empty()
                || policy.wallet_ids.len() > MAX_POLICY_VALUES
                || policy.purposes.len() > MAX_POLICY_VALUES
                || policy
                    .wallet_ids
                    .iter()
                    .any(|wallet_id| validate_identifier("wallet_id", wallet_id).is_err())
                || policy
                    .purposes
                    .iter()
                    .any(|purpose| validate_identifier("purpose", purpose).is_err())
            {
                return Err(Error::Config(
                    "each client policy requires a safe prefix and non-empty, safe wallet_ids and purposes".to_owned(),
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
        {
            return Err(Error::Config(
                "request/dedup limits exceed the supported safety bounds".to_owned(),
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
        if self.transports.local.enabled && self.transports.local.socket_path.trim().is_empty() {
            return Err(Error::Config(
                "transports.local.socket_path must not be empty".to_owned(),
            ));
        }
        if self.transports.mqtt.enabled {
            let mqtt = &self.transports.mqtt;
            if mqtt.host.trim().is_empty()
                || mqtt.port == 0
                || validate_identifier("mqtt.client_id", &mqtt.client_id).is_err()
                || validate_identifier("mqtt.consumer_group", &mqtt.consumer_group).is_err()
                || mqtt.request_topic_prefix.trim().is_empty()
                || mqtt.response_topic_prefix.trim().is_empty()
                || mqtt.request_topic_prefix.contains(['+', '#'])
                || mqtt.response_topic_prefix.contains(['+', '#'])
            {
                return Err(Error::Config(
                    "enabled MQTT transport has an invalid required field".to_owned(),
                ));
            }
            if mqtt.qos > 2 {
                return Err(Error::Config("transports.mqtt.qos must be 0, 1, or 2".to_owned()));
            }
        }
        Ok(())
    }

    fn validate_store(&self) -> Result<()> {
        if self.store.directory.trim().is_empty() {
            return Err(Error::Config("store.directory must not be empty".to_owned()));
        }
        if self.store.master_key_file.trim().is_empty() {
            return Err(Error::Config("store.master_key_file must not be empty".to_owned()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_config_without_transport() {
        let config: Config = toml::from_str(
            r#"
                [store]
                directory = "/tmp/wallet"
                master_key_file = "/tmp/master.key"
                [transports]
            "#,
        )
        .unwrap();
        assert!(config.validate().is_err());
    }

    #[test]
    fn loads_store_only_config_for_offline_commands() {
        let temp = tempfile::TempDir::new().unwrap();
        let path = temp.path().join("wallet.toml");
        std::fs::write(
            &path,
            r#"
                [store]
                directory = "/tmp/wallet"
                master_key_file = "/tmp/master.key"
            "#,
        )
        .unwrap();
        assert!(Config::load(path).is_ok());
    }

    #[test]
    fn rejects_config_without_client_policy() {
        let config: Config = toml::from_str(
            r#"
                [store]
                directory = "/tmp/wallet"
                master_key_file = "/tmp/master.key"

                [transports.local]
                enabled = true
                socket_path = "/tmp/wallet.sock"
            "#,
        )
        .unwrap();
        assert!(matches!(config.validate(), Err(Error::Config(message)) if message.contains("client_policies")));
    }
}
