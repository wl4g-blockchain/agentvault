use std::{sync::Arc, time::Duration};

use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS, Transport};
use zeroize::Zeroizing;

use crate::{
    config::MqttConfig,
    protocol::{validate_identifier, SignRequest},
    secret::read_private_text,
    Error, Result, WalletService,
};

const MAX_PASSWORD_FILE_BYTES: usize = 16 * 1024;

/// Serves signing requests through a reconnecting shared MQTT subscription.
///
/// # Errors
///
/// Returns an error when credentials are unsafe or MQTT setup/subscription fails.
pub async fn run(config: MqttConfig, service: Arc<WalletService>) -> Result<()> {
    let mut options = MqttOptions::new(&config.client_id, &config.host, config.port);
    options.set_keep_alive(Duration::from_secs(30));
    options.set_clean_session(true);
    if config.tls {
        options.set_transport(Transport::tls_with_default_config());
    }
    if !config.username.is_empty() {
        let password = read_password(&config.password_file)?;
        options.set_credentials(config.username.clone(), password.as_str());
    }

    let qos = qos(config.qos);
    let (client, mut event_loop) = AsyncClient::new(options, 32);
    let request_topic_prefix = config.request_topic_prefix.trim_end_matches('/').to_owned();
    let subscription = format!("$share/{}/{}/+", config.consumer_group, request_topic_prefix);

    loop {
        match event_loop.poll().await {
            Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                client
                    .subscribe(&subscription, qos)
                    .await
                    .map_err(|error| Error::Mqtt(error.to_string()))?;
                tracing::info!(
                    host = %config.host,
                    port = config.port,
                    topic = %subscription,
                    "MQTT signing transport connected"
                );
            }
            Ok(Event::Incoming(Incoming::Publish(message))) => {
                handle_publish(
                    &client,
                    &config,
                    &service,
                    &request_topic_prefix,
                    &message.topic,
                    message.payload.as_ref(),
                    qos,
                )
                .await;
            }
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(%error, "MQTT connection interrupted; retrying");
                tokio::time::sleep(Duration::from_secs(1)).await;
            }
        }
    }
}

async fn handle_publish(
    client: &AsyncClient,
    config: &MqttConfig,
    service: &WalletService,
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
    let request = match serde_json::from_slice::<SignRequest>(payload) {
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
            "discarding signing request whose topic and client_id differ"
        );
        return;
    }
    let response = service.handle(&request);
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
    fn maps_configured_qos() {
        assert_eq!(qos(0), QoS::AtMostOnce);
        assert_eq!(qos(1), QoS::AtLeastOnce);
        assert_eq!(qos(2), QoS::ExactlyOnce);
    }

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
