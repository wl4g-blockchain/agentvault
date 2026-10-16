use std::{fmt, net::IpAddr, str::FromStr as _, time::Duration};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand_core::{OsRng, RngCore as _};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, QoS, Transport};
use serde::{Deserialize, Serialize};
use tokio::time::timeout;
use zeroize::Zeroizing;

use crate::{paths, HttpMethod, MqttConfig, TransportError, TransportRequest, TransportResponse, VaultTransport};

/// Authenticated, client-scoped MQTT transport for outbound payment
/// authorization. The broker ACL must bind `username` to `vault_client_id`;
/// neither field is accepted from an Agent payment request.
#[derive(Clone)]
pub struct MqttVaultTransport {
    host: String,
    port: u16,
    tls: bool,
    username: String,
    password: Zeroizing<String>,
    connection_prefix: String,
    vault_client_id: String,
    request_timeout: Duration,
    max_response_bytes: usize,
}

impl fmt::Debug for MqttVaultTransport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MqttVaultTransport")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("username", &self.username)
            .field("password", &"<redacted>")
            .field("connection_prefix", &self.connection_prefix)
            .field("vault_client_id", &self.vault_client_id)
            .field("request_timeout", &self.request_timeout)
            .field("max_response_bytes", &self.max_response_bytes)
            .finish()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestEnvelope<'a> {
    #[serde(rename = "type")]
    message_type: &'static str,
    version: u8,
    correlation_id: &'a str,
    request: WireRequest<'a>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct WireRequest<'a> {
    method: &'static str,
    path: &'a str,
    headers: &'a std::collections::BTreeMap<String, String>,
    body: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ResponseEnvelope {
    #[serde(rename = "type")]
    message_type: String,
    version: u8,
    correlation_id: String,
    status: u16,
    #[serde(default)]
    headers: std::collections::BTreeMap<String, String>,
    body: String,
}

impl MqttVaultTransport {
    /// Builds a payment transport. Plain MQTT is accepted only on loopback.
    ///
    /// # Errors
    ///
    /// Rejects blank/unsafe identifiers, credentials, zero ports/timeouts, or
    /// cleartext remote brokers.
    pub fn new(config: MqttConfig) -> Result<Self, TransportError> {
        let transport = Self {
            host: config.host,
            port: config.port,
            tls: config.tls,
            username: config.username,
            password: Zeroizing::new(config.password),
            connection_prefix: config.connection_prefix,
            vault_client_id: config.vault_client_id,
            request_timeout: config.request_timeout,
            max_response_bytes: config.max_response_bytes,
        };
        if transport.port == 0
            || transport.request_timeout.is_zero()
            || transport.max_response_bytes == 0
            || !valid_identifier(&transport.username)
            || transport.password.is_empty()
            || !valid_identifier(&transport.connection_prefix)
            || !valid_identifier(&transport.vault_client_id)
            || transport.host.trim().is_empty()
            || (!transport.tls && !is_loopback(&transport.host))
        {
            return Err(TransportError::new("invalid or unsafe AgentVault MQTT configuration"));
        }
        Ok(transport)
    }

    async fn execute(&self, request: &TransportRequest) -> Result<TransportResponse, TransportError> {
        let operation = mqtt_operation(request)?;
        let correlation_id = random_id();
        let connection_id = format!("{}-{}", self.connection_prefix, &correlation_id[..12]);
        let mut options = MqttOptions::new(connection_id, &self.host, self.port);
        options.set_keep_alive(Duration::from_secs(15));
        options.set_clean_session(true);
        options.set_credentials(&self.username, self.password.as_str());
        if self.tls {
            options.set_transport(Transport::tls_with_default_config());
        }
        let command_topic = format!(
            "agentvault/v1/clients/{}/commands/{}",
            self.vault_client_id, operation.command
        );
        let result_filter = format!("agentvault/v1/clients/{}/results/+", self.vault_client_id);
        let result_topic = format!(
            "agentvault/v1/clients/{}/results/{correlation_id}",
            self.vault_client_id
        );
        let envelope = RequestEnvelope {
            message_type: operation.message_type,
            version: 1,
            correlation_id: &correlation_id,
            request: WireRequest {
                method: "POST",
                path: &request.path,
                headers: &request.headers,
                body: URL_SAFE_NO_PAD.encode(&request.body),
            },
        };
        let payload = serde_json::to_vec(&envelope)
            .map_err(|error| TransportError::new(format!("encode MQTT request: {error}")))?;
        let (client, mut event_loop) = AsyncClient::new(options, 16);
        let mut published = false;
        loop {
            match event_loop.poll().await {
                Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                    client
                        .subscribe(&result_filter, QoS::AtLeastOnce)
                        .await
                        .map_err(|error| TransportError::new(format!("subscribe MQTT result: {error}")))?;
                }
                Ok(Event::Incoming(Incoming::SubAck(_))) if !published => {
                    client
                        .publish(&command_topic, QoS::AtLeastOnce, false, payload.clone())
                        .await
                        .map_err(|error| TransportError::new(format!("publish MQTT command: {error}")))?;
                    published = true;
                }
                Ok(Event::Incoming(Incoming::Publish(message))) if message.topic == result_topic => {
                    if message.payload.len() > self.max_response_bytes {
                        return Err(TransportError::response_too_large(
                            "AgentVault MQTT response is too large",
                        ));
                    }
                    let envelope: ResponseEnvelope = serde_json::from_slice(&message.payload)
                        .map_err(|_| TransportError::new("invalid AgentVault MQTT response"))?;
                    if envelope.version != 1
                        || !response_type_matches(&envelope.message_type, envelope.status, operation.response_type)
                        || envelope.correlation_id != correlation_id
                        || !(100..=599).contains(&envelope.status)
                    {
                        return Err(TransportError::new("mismatched AgentVault MQTT response"));
                    }
                    let body = URL_SAFE_NO_PAD
                        .decode(envelope.body)
                        .map_err(|_| TransportError::new("invalid AgentVault MQTT response body"))?;
                    if body.len() > self.max_response_bytes {
                        return Err(TransportError::response_too_large(
                            "AgentVault MQTT response is too large",
                        ));
                    }
                    return Ok(TransportResponse {
                        status: envelope.status,
                        headers: envelope.headers,
                        body,
                    });
                }
                Ok(_) => {}
                Err(error) => return Err(TransportError::new(format!("AgentVault MQTT connection: {error}"))),
            }
        }
    }
}

fn response_type_matches(actual: &str, status: u16, expected: &str) -> bool {
    if status >= 400 {
        actual == "payment.error"
    } else {
        actual == expected
    }
}

struct MqttOperation {
    command: &'static str,
    message_type: &'static str,
    response_type: &'static str,
}

fn mqtt_operation(request: &TransportRequest) -> Result<MqttOperation, TransportError> {
    if request.method != HttpMethod::Post {
        return Err(TransportError::new("MQTT payment operation must use POST"));
    }
    if request.path == paths::AUTHORIZE_PAYMENT {
        return Ok(MqttOperation {
            command: "authorize",
            message_type: "payment.authorize",
            response_type: "payment.authorization_result",
        });
    }
    let payee = match request.path.as_str() {
        paths::RESOLVE_OFFER => Some(("resolve-offer", "payment.resolve_offer", "payment.offer_resolved")),
        paths::CREATE_CHALLENGE => Some((
            "create-challenge",
            "payment.create_challenge",
            "payment.challenge_created",
        )),
        paths::VERIFY_PAYMENT => Some(("verify-payment", "payment.verify", "payment.verification_result")),
        paths::SETTLE_PAYMENT => Some(("settle-payment", "payment.settle", "payment.settlement_result")),
        _ => None,
    };
    if let Some((command, message_type, response_type)) = payee {
        return Ok(MqttOperation {
            command,
            message_type,
            response_type,
        });
    }
    let intent_id = request
        .path
        .strip_prefix("/api/v1/payments/")
        .and_then(|value| value.strip_suffix("/outbound-response"));
    if intent_id.is_some_and(|value| valid_identifier(value) && !value.contains('/')) {
        return Ok(MqttOperation {
            command: "outbound-response",
            message_type: "payment.outbound_response",
            response_type: "payment.outbound_response_result",
        });
    }
    Err(TransportError::new(
        "MQTT transport does not support this payment operation",
    ))
}

impl VaultTransport for MqttVaultTransport {
    async fn send(&self, request: TransportRequest) -> Result<TransportResponse, TransportError> {
        timeout(self.request_timeout, self.execute(&request))
            .await
            .map_err(|_| TransportError::timeout("AgentVault MQTT request timed out"))?
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn is_loopback(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost") || IpAddr::from_str(host).is_ok_and(|address| address.is_loopback())
}

fn random_id() -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut random = [0_u8; 16];
    OsRng.fill_bytes(&mut random);
    let mut encoded = String::with_capacity(32);
    for byte in random {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
#[path = "../../tests/transport/mqtt_test.rs"]
mod tests;
