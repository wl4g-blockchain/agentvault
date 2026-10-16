use std::{fmt, time::Duration};

use crate::{AssetIdentity, Error, Result};

const DEFAULT_APPROVAL_TIMEOUT: Duration = Duration::from_secs(120);
const DEFAULT_APPROVAL_POLL_INTERVAL: Duration = Duration::from_secs(1);
const DEFAULT_MAX_CHALLENGE_BYTES: usize = 256 * 1024;
const DEFAULT_MAX_MESSAGE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone)]
pub struct GrpcConfig {
    pub endpoint: String,
    pub tls: bool,
    pub authguard_bearer: Option<String>,
    pub request_timeout: Duration,
    pub max_message_bytes: usize,
}

impl GrpcConfig {
    #[must_use]
    pub fn new(endpoint: impl Into<String>) -> Self {
        Self {
            endpoint: endpoint.into(),
            tls: true,
            authguard_bearer: None,
            request_timeout: Duration::from_secs(30),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
        }
    }

    #[must_use]
    pub fn with_authguard_bearer(mut self, token: impl Into<String>) -> Self {
        self.authguard_bearer = Some(token.into());
        self
    }

    #[must_use]
    pub const fn with_tls(mut self, tls: bool) -> Self {
        self.tls = tls;
        self
    }
}

impl fmt::Debug for GrpcConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GrpcConfig")
            .field("endpoint", &self.endpoint)
            .field("tls", &self.tls)
            .field(
                "authguard_bearer",
                &self.authguard_bearer.as_ref().map(|_| "<redacted>"),
            )
            .field("request_timeout", &self.request_timeout)
            .field("max_message_bytes", &self.max_message_bytes)
            .finish()
    }
}

#[derive(Clone)]
pub struct MqttConfig {
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub username: String,
    pub password: String,
    pub connection_prefix: String,
    pub vault_client_id: String,
    pub request_timeout: Duration,
    pub max_response_bytes: usize,
}

impl fmt::Debug for MqttConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MqttConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("username", &"<redacted>")
            .field("password", &"<redacted>")
            .field("connection_prefix", &self.connection_prefix)
            .field("vault_client_id", &self.vault_client_id)
            .field("request_timeout", &self.request_timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayerConfig {
    pub scenario_id: String,
    pub wallet_id: String,
    pub protocol: String,
    pub asset: AssetIdentity,
    pub amount: String,
    pub recipient: String,
    pub approval_timeout: Duration,
    pub approval_poll_interval: Duration,
    pub delivery_retries: u8,
    pub max_challenge_bytes: usize,
}

impl PayerConfig {
    pub fn new(
        scenario_id: impl Into<String>,
        wallet_id: impl Into<String>,
        protocol: impl Into<String>,
        asset: AssetIdentity,
        amount: impl Into<String>,
        recipient: impl Into<String>,
    ) -> Result<Self> {
        let config = Self {
            scenario_id: scenario_id.into(),
            wallet_id: wallet_id.into(),
            protocol: protocol.into(),
            asset,
            amount: amount.into(),
            recipient: recipient.into(),
            approval_timeout: DEFAULT_APPROVAL_TIMEOUT,
            approval_poll_interval: DEFAULT_APPROVAL_POLL_INTERVAL,
            delivery_retries: 0,
            max_challenge_bytes: DEFAULT_MAX_CHALLENGE_BYTES,
        };
        config.validate()?;
        Ok(config)
    }

    #[must_use]
    pub const fn with_approval_wait(mut self, timeout: Duration, poll_interval: Duration) -> Self {
        self.approval_timeout = timeout;
        self.approval_poll_interval = poll_interval;
        self
    }

    #[must_use]
    pub const fn with_delivery_retries(mut self, retries: u8) -> Self {
        self.delivery_retries = retries;
        self
    }

    #[must_use]
    pub const fn with_max_challenge_bytes(mut self, max_challenge_bytes: usize) -> Self {
        self.max_challenge_bytes = max_challenge_bytes;
        self
    }

    pub(crate) fn validate(&self) -> Result<()> {
        for (name, value) in [
            ("scenario_id", self.scenario_id.as_str()),
            ("wallet_id", self.wallet_id.as_str()),
            ("protocol", self.protocol.as_str()),
            ("asset.network", self.asset.network.as_str()),
            ("asset.asset", self.asset.asset.as_str()),
            ("amount", self.amount.as_str()),
            ("recipient", self.recipient.as_str()),
        ] {
            if value.trim().is_empty() {
                return Err(Error::InvalidArgument(format!("{name} cannot be blank")));
            }
        }
        if self.approval_timeout.is_zero()
            || self.approval_poll_interval.is_zero()
            || self.approval_poll_interval > self.approval_timeout
            || self.delivery_retries > 3
            || self.max_challenge_bytes == 0
        {
            return Err(Error::InvalidArgument(
                "payer middleware retry, timeout, or challenge limits are invalid".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PayeeConfig {
    pub max_body_bytes: usize,
}

impl Default for PayeeConfig {
    fn default() -> Self {
        Self {
            max_body_bytes: 1024 * 1024,
        }
    }
}

impl PayeeConfig {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.max_body_bytes == 0 {
            return Err(Error::InvalidArgument(
                "payee middleware body limit must be positive".to_owned(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
#[path = "../../tests/config/config_test.rs"]
mod tests;
