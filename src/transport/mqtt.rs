use std::{sync::Arc, time::Duration};

use anyhow::Context as _;
use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS, Transport};
use zeroize::Zeroizing;

use crate::{
    config::MqttConfig,
    model::sign::{validate_identifier, PaymentSignRequest, SignTransport},
    observability::AgentVaultMetrics,
    secret::read_private_text,
    server::{RuntimeComponent, Shutdown},
    service::PaymentSignService,
    Error, Result,
};

const MAX_PASSWORD_FILE_BYTES: usize = 16 * 1024;

/// Reconnecting shared-subscription MQTT signing endpoint.
pub struct MqttSignServer {
    config: MqttConfig,
    service: Arc<PaymentSignService>,
    metrics: AgentVaultMetrics,
}

impl MqttSignServer {
    #[must_use]
    pub fn new(config: MqttConfig, service: Arc<PaymentSignService>, metrics: AgentVaultMetrics) -> Self {
        Self {
            config,
            service,
            metrics,
        }
    }

    async fn serve(self, mut shutdown: Shutdown) -> Result<()> {
        let mut options = MqttOptions::new(&self.config.client_id, &self.config.host, self.config.port);
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
        let (client, mut event_loop) = AsyncClient::new(options, 32);
        let request_topic_prefix = self.config.request_topic_prefix.trim_end_matches('/').to_owned();
        let subscription = format!("$share/{}/{}/+", self.config.consumer_group, request_topic_prefix);

        loop {
            let event = tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                event = event_loop.poll() => event,
            };
            match event {
                Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                    client
                        .subscribe(&subscription, qos)
                        .await
                        .map_err(|error| Error::Mqtt(error.to_string()))?;
                    tracing::info!(
                        host = %self.config.host,
                        port = self.config.port,
                        topic = %subscription,
                        "MQTT signing endpoint connected"
                    );
                }
                Ok(Event::Incoming(Incoming::Publish(message))) => {
                    handle_publish(
                        &client,
                        &self.config,
                        &self.service,
                        &request_topic_prefix,
                        &message.topic,
                        message.payload.as_ref(),
                        qos,
                    )
                    .await;
                }
                Ok(_) => {}
                Err(error) => {
                    self.metrics.mqtt_reconnected();
                    tracing::warn!(%error, "MQTT connection interrupted; retrying");
                    tokio::select! {
                        () = shutdown.cancelled() => return Ok(()),
                        () = tokio::time::sleep(self.config.reconnect_delay) => {}
                    }
                }
            }
        }
    }
}

#[async_trait]
impl RuntimeComponent for MqttSignServer {
    fn name(&self) -> &'static str {
        "MQTT signing endpoint"
    }

    async fn run(self: Box<Self>, shutdown: Shutdown) -> anyhow::Result<()> {
        self.serve(shutdown).await.context("serve MQTT signing endpoint")
    }
}

async fn handle_publish(
    client: &AsyncClient,
    config: &MqttConfig,
    service: &PaymentSignService,
    request_topic_prefix: &str,
    request_topic: &str,
    payload: &[u8],
    qos: QoS,
) {
    if payload.len() > service.max_request_bytes() {
        tracing::warn!(
            payload_bytes = payload.len(),
            "discarding oversized MQTT signing request"
        );
        return;
    }
    let request = match serde_json::from_slice::<PaymentSignRequest>(payload) {
        Ok(request) => request,
        Err(error) => {
            tracing::warn!(%error, "discarding malformed MQTT signing request");
            return;
        }
    };
    if validate_identifier("client_id", &request.client_id).is_err() {
        tracing::warn!("discarding signing request with unsafe client_id");
        return;
    }
    if request_topic_client(request_topic_prefix, request_topic) != Some(request.client_id.as_str()) {
        tracing::warn!(
            topic = request_topic,
            "discarding request whose topic and client_id differ"
        );
        return;
    }
    let response = service.execute(&request, SignTransport::Mqtt);
    let response_topic = format!(
        "{}/{}",
        config.response_topic_prefix.trim_end_matches('/'),
        response.client_id
    );
    let encoded = match serde_json::to_vec(&response) {
        Ok(encoded) => encoded,
        Err(error) => {
            tracing::error!(%error, "failed to serialize signing response");
            return;
        }
    };
    if let Err(error) = client.publish(&response_topic, qos, false, encoded).await {
        tracing::error!(%error, topic = %response_topic, "failed to publish signing response");
    }
}

fn request_topic_client<'a>(prefix: &str, topic: &'a str) -> Option<&'a str> {
    let client_id = topic.strip_prefix(prefix)?.strip_prefix('/')?;
    if client_id.is_empty() || client_id.contains('/') {
        return None;
    }
    Some(client_id)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_only_direct_client_topic() {
        assert_eq!(
            request_topic_client("wallet/v1/sign/requests", "wallet/v1/sign/requests/flowgent-1"),
            Some("flowgent-1")
        );
        assert_eq!(
            request_topic_client("wallet/v1/sign/requests", "wallet/v1/sign/requests/flowgent-1/nested"),
            None
        );
    }
}
