use std::{
    collections::HashMap,
    fmt,
    net::IpAddr,
    str::FromStr as _,
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex as StdMutex,
    },
    time::Duration,
};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, Publish, QoS, SubAck, SubscribeReasonCode, Transport};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::{oneshot, watch, OnceCell},
    task::JoinHandle,
    time::timeout,
};
use zeroize::Zeroizing;

use crate::transport_core::ProducerTraceContext;

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
    closed: Arc<AtomicBool>,
}

struct MqttRuntime {
    client: AsyncClient,
    state: Arc<MqttRuntimeState>,
    task: StdMutex<Option<JoinHandle<()>>>,
}

struct MqttRuntimeState {
    pending: StdMutex<HashMap<String, PendingRequest>>,
    ready: watch::Sender<bool>,
    closed: Arc<AtomicBool>,
    connection_generation: AtomicU64,
    max_response_bytes: usize,
    result_prefix: String,
}

struct PendingRequest {
    expected_type: &'static str,
    command_topic: String,
    payload: Vec<u8>,
    result: oneshot::Sender<Result<TransportResponse, TransportError>>,
    queued_generation: u64,
}

/// Removes a pending reply when its owning Future is dropped or cancelled.
struct PendingReply {
    state: Arc<MqttRuntimeState>,
    request_span_id: String,
}

impl Drop for PendingReply {
    fn drop(&mut self) {
        self.state
            .pending
            .lock()
            .expect("MQTT pending mutex poisoned")
            .remove(&self.request_span_id);
    }
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
            .field("closed", &self.closed.load(Ordering::Acquire))
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
            closed: Arc::new(AtomicBool::new(false)),
        };
        if transport.port == 0
            || transport.request_timeout.is_zero()
            || transport.max_response_bytes == 0
            || !MqttVaultTransport::valid_identifier(&transport.username)
            || transport.password.is_empty()
            || !MqttVaultTransport::valid_identifier(&transport.connection_prefix)
            || !MqttVaultTransport::valid_identifier(&transport.vault_client_id)
            || transport.host.trim().is_empty()
            || (!transport.tls && !MqttVaultTransport::is_loopback(&transport.host))
        {
            return Err(TransportError::new("invalid or unsafe AgentVault MQTT configuration"));
        }
        Ok(transport)
    }

    async fn runtime(&self) -> &MqttRuntime {
        self.runtime.get_or_init(|| async { MqttRuntime::start(self) }).await
    }

    async fn execute(&self, request: &TransportRequest) -> Result<TransportResponse, TransportError> {
        if self.closed.load(Ordering::Acquire) {
            return Err(TransportError::new("AgentVault MQTT transport is closed"));
        }
        let operation = MqttOperation::for_request(request)?;
        let trace = ProducerTraceContext::new(&request.headers);
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
            let mut pending = runtime.state.pending.lock().expect("MQTT pending mutex poisoned");
            if self.closed.load(Ordering::Acquire) {
                return Err(TransportError::new("AgentVault MQTT transport is closed"));
            }
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
                    queued_generation: 0,
                },
            );
        }
        let _reply = PendingReply {
            state: Arc::clone(&runtime.state),
            request_span_id: request_span_id.clone(),
        };
        if *runtime.state.ready.borrow() {
            runtime.state.publish_pending(&runtime.client);
        }
        let outcome = timeout(self.request_timeout, receiver).await;
        match outcome {
            Err(_) => Err(TransportError::timeout("AgentVault MQTT request timed out")),
            Ok(Err(_)) => Err(TransportError::new("AgentVault MQTT response task stopped")),
            Ok(Ok(result)) => result,
        }
    }
}

impl MqttRuntime {
    fn start(transport: &MqttVaultTransport) -> Self {
        let connection_id = format!(
            "{}-{}",
            transport.connection_prefix,
            ProducerTraceContext::random_hex(6)
        );
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
            pending: StdMutex::new(HashMap::new()),
            ready,
            closed: Arc::clone(&transport.closed),
            connection_generation: AtomicU64::new(0),
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

    fn close(&self) {
        self.state.closed.store(true, Ordering::Release);
        self.state.ready.send_replace(false);
        let requests = {
            let mut pending = self.state.pending.lock().expect("MQTT pending mutex poisoned");
            pending.drain().map(|(_, request)| request).collect::<Vec<_>>()
        };
        for request in requests {
            let _ = request
                .result
                .send(Err(TransportError::new("AgentVault MQTT transport is closed")));
        }
        let _ = self.client.try_disconnect();
        if let Some(task) = self.task.lock().expect("MQTT task mutex poisoned").take() {
            task.abort();
        }
    }
}

impl Drop for MqttRuntime {
    fn drop(&mut self) {
        self.state.closed.store(true, Ordering::Release);
        if let Some(task) = self.task.get_mut().expect("MQTT task mutex poisoned").take() {
            task.abort();
        }
    }
}

impl MqttRuntimeState {
    fn publish_pending(&self, client: &AsyncClient) {
        if self.closed.load(Ordering::Acquire) || !*self.ready.borrow() {
            return;
        }
        let generation = self.connection_generation.load(Ordering::Acquire);
        let mut pending = self.pending.lock().expect("MQTT pending mutex poisoned");
        for request in pending.values_mut() {
            if request.result.is_closed() || request.queued_generation == generation {
                continue;
            }
            if client
                .try_publish(&request.command_topic, QoS::AtLeastOnce, false, request.payload.clone())
                .is_err()
            {
                // Polling the event loop drains its queue; retry on the next turn.
                break;
            }
            request.queued_generation = generation;
        }
    }

    async fn run(state: Arc<Self>, client: AsyncClient, mut event_loop: rumqttc::EventLoop, result_filter: String) {
        let mut subscription_pending = false;
        while !state.closed.load(Ordering::Acquire) {
            if subscription_pending && client.try_subscribe(&result_filter, QoS::AtLeastOnce).is_ok() {
                subscription_pending = false;
            }
            state.publish_pending(&client);
            match event_loop.poll().await {
                Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                    state.ready.send_replace(false);
                    subscription_pending = true;
                }
                Ok(Event::Incoming(Incoming::SubAck(ack))) => {
                    if !MqttRuntimeState::subscription_granted(&ack) {
                        state.fail_all(&TransportError::new(
                            "AgentVault MQTT result subscription was rejected or downgraded",
                        ));
                        continue;
                    }
                    state.connection_generation.fetch_add(1, Ordering::AcqRel);
                    state.ready.send_replace(true);
                }
                Ok(Event::Incoming(Incoming::Publish(message))) => {
                    state.handle_response(&message);
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

    fn handle_response(&self, message: &Publish) {
        if message.qos != QoS::AtLeastOnce || message.retain || message.payload.len() > self.max_response_bytes {
            return;
        }
        let Ok(envelope) = serde_json::from_slice::<ResponseEnvelope>(&message.payload) else {
            return;
        };
        let expected_topic = format!("{}/{}", self.result_prefix, envelope.request_span_id);
        let mut pending = self.pending.lock().expect("MQTT pending mutex poisoned");
        let Some(request) = pending.get(&envelope.request_span_id) else {
            return;
        };
        if message.topic != expected_topic
            || envelope.version != 1
            || !(100..=599).contains(&envelope.status)
            || !MqttRuntimeState::response_type_matches(&envelope.message_type, envelope.status, request.expected_type)
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

    fn fail_all(&self, error: &TransportError) {
        self.ready.send_replace(false);
        let requests = {
            let mut pending = self.pending.lock().expect("MQTT pending mutex poisoned");
            pending.drain().map(|(_, request)| request).collect::<Vec<_>>()
        };
        for request in requests {
            let _ = request.result.send(Err(error.clone()));
        }
    }
}

struct MqttOperation {
    command: &'static str,
    message_type: &'static str,
    response_type: &'static str,
}

impl VaultTransport for MqttVaultTransport {
    async fn send(&self, request: TransportRequest) -> Result<TransportResponse, TransportError> {
        self.execute(&request).await
    }

    async fn close(&self) -> Result<(), TransportError> {
        self.closed.store(true, Ordering::Release);
        if let Some(runtime) = self.runtime.get() {
            runtime.close();
            Ok(())
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
#[path = "../../tests/transport/mqtt_test.rs"]
mod tests;

impl MqttRuntimeState {
    fn subscription_granted(ack: &SubAck) -> bool {
        matches!(
            ack.return_codes.as_slice(),
            [SubscribeReasonCode::Success(QoS::AtLeastOnce)]
        )
    }
}

impl MqttRuntimeState {
    fn response_type_matches(actual: &str, status: u16, expected: &str) -> bool {
        if status >= 400 {
            actual == "payment.error"
        } else {
            actual == expected
        }
    }
}

impl MqttOperation {
    fn for_request(request: &TransportRequest) -> Result<MqttOperation, TransportError> {
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
            paths::RESOLVE_PRICE => Some(("resolve-price", "payment.resolve_price", "payment.price_resolved")),
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
        if intent_id.is_some_and(|value| MqttVaultTransport::valid_identifier(value) && !value.contains('/')) {
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
}

impl MqttVaultTransport {
    fn valid_identifier(value: &str) -> bool {
        !value.is_empty()
            && value.len() <= 128
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    }
}

impl MqttVaultTransport {
    fn is_loopback(host: &str) -> bool {
        host.eq_ignore_ascii_case("localhost") || IpAddr::from_str(host).is_ok_and(|address| address.is_loopback())
    }
}
