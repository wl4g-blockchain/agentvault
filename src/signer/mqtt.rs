use std::time::Duration;

use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS, Transport};
use tokio::time::timeout;
use zeroize::Zeroizing;

use crate::{
    config::MqttConfig,
    model::sign::{validate_identifier, PaymentSignRequest, PaymentSignResponse},
    secret::read_private_text,
    signer::IPaymentSigner,
    Error, Result,
};

const MAX_PASSWORD_FILE_BYTES: usize = 16 * 1024;

/// Request/response MQTT implementation for remote payment agents.
#[derive(Clone, Debug)]
pub struct MqttPaymentSigner {
    config: MqttConfig,
}

impl MqttPaymentSigner {
    #[must_use]
    pub fn new(config: MqttConfig) -> Self {
        Self { config }
    }

    async fn execute_inner(&self, request: &PaymentSignRequest) -> Result<PaymentSignResponse> {
        validate_identifier("client_id", &request.client_id)?;
        let suffix = UuidSuffix::new();
        let connection_id = format!("{}-{suffix}", request.client_id);
        let mut options = MqttOptions::new(connection_id, &self.config.host, self.config.port);
        options.set_keep_alive(Duration::from_secs(30));
        options.set_clean_session(true);
        if self.config.tls {
            options.set_transport(Transport::tls_with_default_config());
        }
        if !self.config.username.is_empty() {
            let password = read_password(&self.config.password_file)?;
            options.set_credentials(self.config.username.clone(), password.as_str());
        }

        let qos = qos(self.config.qos);
        let request_topic = format!(
            "{}/{}",
            self.config.request_topic_prefix.trim_end_matches('/'),
            request.client_id
        );
        let response_topic = format!(
            "{}/{}",
            self.config.response_topic_prefix.trim_end_matches('/'),
            request.client_id
        );
        let (client, mut event_loop) = AsyncClient::new(options, 16);
        let payload = serde_json::to_vec(request)?;
        let mut published = false;

        loop {
            match event_loop.poll().await {
                Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                    client
                        .subscribe(&response_topic, qos)
                        .await
                        .map_err(|error| Error::Mqtt(error.to_string()))?;
                }
                Ok(Event::Incoming(Incoming::SubAck(_))) if !published => {
                    client
                        .publish(&request_topic, qos, false, payload.clone())
                        .await
                        .map_err(|error| Error::Mqtt(error.to_string()))?;
                    published = true;
                }
                Ok(Event::Incoming(Incoming::Publish(message))) if message.topic == response_topic => {
                    let response: PaymentSignResponse = serde_json::from_slice(&message.payload)?;
                    if response.request_id == request.request_id {
                        return Ok(response);
                    }
                }
                Ok(_) => {}
                Err(error) => return Err(Error::Mqtt(error.to_string())),
            }
        }
    }
}

#[async_trait]
impl IPaymentSigner for MqttPaymentSigner {
    async fn execute(&self, request: PaymentSignRequest) -> Result<PaymentSignResponse> {
        timeout(self.config.request_timeout, self.execute_inner(&request))
            .await
            .map_err(|_| Error::Timeout("MQTT payment signer".to_owned()))?
    }
}

struct UuidSuffix(String);

impl UuidSuffix {
    fn new() -> Self {
        let value = uuid::Uuid::new_v4().simple().to_string();
        Self(value[..12].to_owned())
    }
}

impl std::fmt::Display for UuidSuffix {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

fn qos(value: u8) -> QoS {
    match value {
        0 => QoS::AtMostOnce,
        2 => QoS::ExactlyOnce,
        _ => QoS::AtLeastOnce,
    }
}

fn read_password(path: &str) -> Result<Zeroizing<String>> {
    if path.is_empty() {
        return Ok(Zeroizing::new(String::new()));
    }
    read_private_text(path, "MQTT password file", MAX_PASSWORD_FILE_BYTES)
        .map(|password| Zeroizing::new(password.trim_end_matches(['\r', '\n']).to_owned()))
}
