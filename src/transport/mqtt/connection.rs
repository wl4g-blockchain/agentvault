//! Immutable broker connection settings and subscription validation.
use super::*;
#[derive(Clone)]
pub(crate) struct MqttSettings {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) client_id: String,
    pub(crate) consumer_group: String,
    pub(crate) topic_prefix: String,
    pub(crate) tls: bool,
    pub(crate) username: String,
    pub(crate) password: Zeroizing<String>,
    pub(crate) request_timeout: Duration,
    pub(crate) reconnect_delay: Duration,
}

impl MqttSettings {
    pub(crate) fn clone_for_task(&self) -> OutboxSettings {
        OutboxSettings {
            topic_prefix: self.topic_prefix.clone(),
            lease_owner: format!("mqtt:{}", self.client_id),
            request_timeout: self.request_timeout,
        }
    }

    pub(crate) fn payment_event_dispatcher_config(&self) -> PaymentEventDispatcherConfig {
        PaymentEventDispatcherConfig {
            worker_id: format!("mqtt:{}:payment-events", self.client_id),
            batch_size: OUTBOX_BATCH_SIZE,
            lease_duration: OUTBOX_LEASE_TTL,
            delivery_timeout: self.request_timeout.max(Duration::from_secs(1)),
            poll_interval: Duration::from_secs(1),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_mins(5),
            max_attempts: 32,
        }
    }
}

impl fmt::Debug for MqttSettings {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MqttSettings")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("client_id", &self.client_id)
            .field("consumer_group", &self.consumer_group)
            .field("topic_prefix", &self.topic_prefix)
            .field("tls", &self.tls)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("request_timeout", &self.request_timeout)
            .field("reconnect_delay", &self.reconnect_delay)
            .finish()
    }
}

impl MqttSettings {
    pub(crate) fn command_subscription_filters(prefix: &str, consumer_group: &str) -> [String; 6] {
        [
            MqttSettings::shared_subscription_filter(consumer_group, &format!("{prefix}/+/commands/authorize")),
            MqttSettings::shared_subscription_filter(consumer_group, &format!("{prefix}/+/commands/outbound-response")),
            MqttSettings::shared_subscription_filter(consumer_group, &format!("{prefix}/+/commands/resolve-price")),
            MqttSettings::shared_subscription_filter(consumer_group, &format!("{prefix}/+/commands/create-challenge")),
            MqttSettings::shared_subscription_filter(consumer_group, &format!("{prefix}/+/commands/verify-payment")),
            MqttSettings::shared_subscription_filter(consumer_group, &format!("{prefix}/+/commands/settle-payment")),
        ]
    }

    pub(crate) fn shared_subscription_filter(consumer_group: &str, filter: &str) -> String {
        format!("$share/{consumer_group}/{filter}")
    }

    pub(crate) fn validate_subscription_ack(ack: &SubAck, expected: usize) -> anyhow::Result<()> {
        if ack.return_codes.len() != expected
            || ack
                .return_codes
                .iter()
                .any(|code| !matches!(code, SubscribeReasonCode::Success(QoS::AtLeastOnce)))
        {
            anyhow::bail!("MQTT broker rejected one or more AgentVault command subscriptions");
        }
        Ok(())
    }

    pub(crate) fn mqtt_options(settings: &MqttSettings) -> MqttOptions {
        let mut options = MqttOptions::new(&settings.client_id, &settings.host, settings.port);
        options.set_keep_alive(Duration::from_secs(15));
        // Every replica owns a unique connection identity and re-subscribes to a
        // shared consumer group after reconnect. Retaining an ephemeral session
        // would leak one broker session per process restart.
        options.set_clean_session(true);
        options.set_credentials(&settings.username, settings.password.as_str());
        if settings.tls {
            options.set_transport(Transport::tls_with_default_config());
        }
        options
    }

    pub(crate) fn validate_config(config: &MqttTransportConfig) -> crate::Result<()> {
        let loopback = config.host.eq_ignore_ascii_case("localhost")
            || IpAddr::from_str(&config.host).is_ok_and(|address| address.is_loopback());
        if !config.enabled
            || config.host.trim().is_empty()
            || config.port == 0
            || !MqttWireCodec::valid_identifier(&config.client_id)
            || !MqttWireCodec::valid_identifier(&config.consumer_group)
            || config.topic_prefix.is_empty()
            || config.topic_prefix.starts_with('/')
            || config.topic_prefix.ends_with('/')
            || config.topic_prefix.contains(['+', '#', '\0'])
            || config.topic_prefix.split('/').any(str::is_empty)
            || config.qos != 1
            || !MqttWireCodec::valid_identifier(&config.username)
            || config.request_timeout.is_zero()
            || config.reconnect_delay.is_zero()
            || (!config.tls && !loopback)
        {
            return Err(Error::Config(
                "enabled MQTT payment transport configuration is invalid".to_owned(),
            ));
        }
        Ok(())
    }

    pub(crate) fn duration_seconds(duration: Duration) -> i64 {
        i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
    }

    pub(crate) fn unix_time() -> crate::Result<i64> {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::InvalidRequest("system clock is before the Unix epoch".to_owned()))?
            .as_secs();
        i64::try_from(seconds).map_err(|_| Error::InvalidRequest("system clock is out of range".to_owned()))
    }
}
#[cfg(test)]
#[path = "../../../tests/transport/mqtt/connection_test.rs"]
mod tests;
