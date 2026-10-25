use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    net::IpAddr,
    str::FromStr as _,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand_core::{OsRng, RngCore as _};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, Publish, QoS, SubAck, SubscribeReasonCode, Transport};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{oneshot, watch, Mutex, OnceCell},
    task::JoinHandle,
    time::timeout,
};
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
    runtime: Arc<OnceCell<MqttRuntime>>,
}

struct MqttRuntime {
    client: AsyncClient,
    state: Arc<MqttRuntimeState>,
    task: StdMutex<Option<JoinHandle<()>>>,
}

struct MqttRuntimeState {
    pending: Mutex<HashMap<String, PendingRequest>>,
    ready: watch::Sender<bool>,
    closed: AtomicBool,
    max_response_bytes: usize,
    result_prefix: String,
}

struct PendingRequest {
    expected_type: &'static str,
    command_topic: String,
    payload: Vec<u8>,
    result: oneshot::Sender<Result<TransportResponse, TransportError>>,
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
            .field("runtime", &"<shared>")
            .finish()
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RequestEnvelope<'a> {
    #[serde(rename = "type")]
    message_type: &'static str,
    version: u8,
    request_span_id: &'a str,
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
    request_span_id: String,
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
            runtime: Arc::new(OnceCell::new()),
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

    async fn runtime(&self) -> &MqttRuntime {
        self.runtime.get_or_init(|| async { MqttRuntime::start(self) }).await
    }

    async fn execute(&self, request: &TransportRequest) -> Result<TransportResponse, TransportError> {
        let operation = mqtt_operation(request)?;
        let trace = producer_trace_context(&request.headers);
        let request_span_id = trace.span_id.clone();
        let command_topic = format!(
            "agentvault/v1/clients/{}/commands/{}",
            self.vault_client_id, operation.command
        );
        tracing::info!(
            event.name = "agentvault.sdk.transport.mqtt.produced",
            trace.id = %trace.trace_id,
            span.id = %trace.span_id,
            parent.id = %trace.parent_span_id.as_deref().unwrap_or(""),
            "publishing an AgentVault MQTT command with W3C Trace Context"
        );
        let envelope = RequestEnvelope {
            message_type: operation.message_type,
            version: 1,
            request_span_id: &request_span_id,
            request: WireRequest {
                method: "POST",
                path: &request.path,
                headers: &trace.headers,
                body: URL_SAFE_NO_PAD.encode(&request.body),
            },
        };
        let payload = serde_json::to_vec(&envelope)
            .map_err(|error| TransportError::new(format!("encode MQTT request: {error}")))?;
        if payload.len() > self.max_response_bytes {
            return Err(TransportError::response_too_large(
                "AgentVault MQTT request exceeds configured limit",
            ));
        }
        let runtime = self.runtime().await;
        if runtime.state.closed.load(Ordering::Acquire) {
            return Err(TransportError::new("AgentVault MQTT transport is closed"));
        }
        let (sender, receiver) = oneshot::channel();
        {
            let mut pending = runtime.state.pending.lock().await;
            if pending.len() >= 256 {
                return Err(TransportError::new("too many pending MQTT payment requests"));
            }
            if pending.contains_key(&request_span_id) {
                return Err(TransportError::new("MQTT requestSpanId collision"));
            }
            pending.insert(
                request_span_id.clone(),
                PendingRequest {
                    expected_type: operation.response_type,
                    command_topic,
                    payload,
                    result: sender,
                },
            );
        }
        if *runtime.state.ready.borrow() {
            runtime.publish_pending(&request_span_id).await;
        }
        let outcome = timeout(self.request_timeout, receiver).await;
        runtime.state.pending.lock().await.remove(&request_span_id);
        match outcome {
            Err(_) => Err(TransportError::timeout("AgentVault MQTT request timed out")),
            Ok(Err(_)) => Err(TransportError::new("AgentVault MQTT response task stopped")),
            Ok(Ok(result)) => result,
        }
    }
}

impl MqttRuntime {
    fn start(transport: &MqttVaultTransport) -> Self {
        let connection_id = format!("{}-{}", transport.connection_prefix, random_hex(6));
        let mut options = MqttOptions::new(connection_id, &transport.host, transport.port);
        options.set_keep_alive(Duration::from_secs(15));
        options.set_clean_session(true);
        options.set_credentials(&transport.username, transport.password.as_str());
        if transport.tls {
            options.set_transport(Transport::tls_with_default_config());
        }
        let (client, event_loop) = AsyncClient::new(options, 512);
        let (ready, _) = watch::channel(false);
        let state = Arc::new(MqttRuntimeState {
            pending: Mutex::new(HashMap::new()),
            ready,
            closed: AtomicBool::new(false),
            max_response_bytes: transport.max_response_bytes,
            result_prefix: format!("agentvault/v1/clients/{}/results", transport.vault_client_id),
        });
        let task_state = Arc::clone(&state);
        let task_client = client.clone();
        let result_filter = format!("{}/+", state.result_prefix);
        let task = tokio::spawn(async move {
            MqttRuntimeState::run(task_state, task_client, event_loop, result_filter).await;
        });
        Self {
            client,
            state,
            task: StdMutex::new(Some(task)),
        }
    }

    async fn publish_pending(&self, request_span_id: &str) {
        let request = {
            let pending = self.state.pending.lock().await;
            pending
                .get(request_span_id)
                .map(|request| (request.command_topic.clone(), request.payload.clone()))
        };
        let Some((topic, payload)) = request else {
            return;
        };
        if let Err(error) = self.client.publish(topic, QoS::AtLeastOnce, false, payload).await {
            tracing::debug!(error = %error, "MQTT publish deferred until reconnect");
        }
    }

    async fn close(&self) -> Result<(), TransportError> {
        if self.state.closed.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        self.state.ready.send_replace(false);
        let requests = {
            let mut pending = self.state.pending.lock().await;
            pending.drain().map(|(_, request)| request).collect::<Vec<_>>()
        };
        for request in requests {
            let _ = request
                .result
                .send(Err(TransportError::new("AgentVault MQTT transport is closed")));
        }
        let _ = self.client.disconnect().await;
        if let Some(task) = self.task.lock().expect("MQTT task mutex poisoned").take() {
            task.abort();
        }
        Ok(())
    }
}

impl MqttRuntimeState {
    async fn run(state: Arc<Self>, client: AsyncClient, mut event_loop: rumqttc::EventLoop, result_filter: String) {
        while !state.closed.load(Ordering::Acquire) {
            match event_loop.poll().await {
                Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                    state.ready.send_replace(false);
                    if let Err(error) = client.subscribe(&result_filter, QoS::AtLeastOnce).await {
                        tracing::warn!(error = %error, "failed to restore MQTT result subscription");
                    }
                }
                Ok(Event::Incoming(Incoming::SubAck(ack))) => {
                    if !subscription_granted(&ack) {
                        state
                            .fail_all(TransportError::new(
                                "AgentVault MQTT result subscription was rejected or downgraded",
                            ))
                            .await;
                        continue;
                    }
                    state.ready.send_replace(true);
                    let requests = {
                        let pending = state.pending.lock().await;
                        pending
                            .values()
                            .map(|request| (request.command_topic.clone(), request.payload.clone()))
                            .collect::<Vec<_>>()
                    };
                    for (topic, payload) in requests {
                        let _ = client.publish(topic, QoS::AtLeastOnce, false, payload).await;
                    }
                }
                Ok(Event::Incoming(Incoming::Publish(message))) => {
                    state.handle_response(message).await;
                }
                Ok(_) => {}
                Err(error) => {
                    state.ready.send_replace(false);
                    tracing::warn!(error = %error, "MQTT connection interrupted; waiting for reconnect");
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            }
        }
    }

    async fn handle_response(&self, message: Publish) {
        if message.qos != QoS::AtLeastOnce || message.retain || message.payload.len() > self.max_response_bytes {
            return;
        }
        let Ok(envelope) = serde_json::from_slice::<ResponseEnvelope>(&message.payload) else {
            return;
        };
        let expected_topic = format!("{}/{}", self.result_prefix, envelope.request_span_id);
        let mut pending = self.pending.lock().await;
        let Some(request) = pending.get(&envelope.request_span_id) else {
            return;
        };
        if message.topic != expected_topic
            || envelope.version != 1
            || !(100..=599).contains(&envelope.status)
            || !response_type_matches(&envelope.message_type, envelope.status, request.expected_type)
        {
            return;
        }
        let Ok(body) = URL_SAFE_NO_PAD.decode(&envelope.body) else {
            return;
        };
        if body.len() > self.max_response_bytes || URL_SAFE_NO_PAD.encode(&body) != envelope.body {
            return;
        }
        let Some(request) = pending.remove(&envelope.request_span_id) else {
            return;
        };
        drop(pending);
        let _ = request.result.send(Ok(TransportResponse {
            status: envelope.status,
            headers: envelope.headers,
            body,
        }));
    }

    async fn fail_all(&self, error: TransportError) {
        self.ready.send_replace(false);
        let requests = {
            let mut pending = self.pending.lock().await;
            pending.drain().map(|(_, request)| request).collect::<Vec<_>>()
        };
        for request in requests {
            let _ = request.result.send(Err(error.clone()));
        }
    }
}

fn subscription_granted(ack: &SubAck) -> bool {
    matches!(
        ack.return_codes.as_slice(),
        [SubscribeReasonCode::Success(QoS::AtLeastOnce)]
    )
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
        self.execute(&request).await
    }

    async fn close(&self) -> Result<(), TransportError> {
        if let Some(runtime) = self.runtime.get() {
            runtime.close().await
        } else {
            Ok(())
        }
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

pub(super) struct ProducerTraceContext {
    pub(super) trace_id: String,
    pub(super) span_id: String,
    pub(super) parent_span_id: Option<String>,
    pub(super) headers: BTreeMap<String, String>,
}

pub(super) fn producer_trace_context(headers: &BTreeMap<String, String>) -> ProducerTraceContext {
    let parent = headers.get("traceparent").and_then(|value| parse_traceparent(value));
    let trace_id = parent
        .as_ref()
        .map_or_else(|| random_hex(16), |(trace_id, _, _)| trace_id.clone());
    let parent_span_id = parent.as_ref().map(|(_, span_id, _)| span_id.clone());
    let flags = parent.as_ref().map_or("01", |(_, _, flags)| flags.as_str());
    let span_id = random_hex(8);
    let mut propagated = headers.clone();
    propagated.insert("traceparent".to_owned(), format!("00-{trace_id}-{span_id}-{flags}"));
    ProducerTraceContext {
        trace_id,
        span_id,
        parent_span_id,
        headers: propagated,
    }
}

fn parse_traceparent(value: &str) -> Option<(String, String, String)> {
    let mut fields = value.split('-');
    let version = fields.next()?;
    let trace_id = fields.next()?;
    let span_id = fields.next()?;
    let flags = fields.next()?;
    if fields.next().is_some()
        || version != "00"
        || !valid_lower_hex(trace_id, 32)
        || trace_id.bytes().all(|byte| byte == b'0')
        || !valid_lower_hex(span_id, 16)
        || span_id.bytes().all(|byte| byte == b'0')
        || !valid_lower_hex(flags, 2)
    {
        return None;
    }
    Some((trace_id.to_owned(), span_id.to_owned(), flags.to_owned()))
}

fn valid_lower_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn random_hex(bytes: usize) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut random = vec![0_u8; bytes];
    loop {
        OsRng.fill_bytes(&mut random);
        if random.iter().any(|byte| *byte != 0) {
            break;
        }
    }
    let mut encoded = String::with_capacity(bytes * 2);
    for byte in random {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

#[cfg(test)]
#[path = "../../tests/transport/mqtt_test.rs"]
mod tests;
