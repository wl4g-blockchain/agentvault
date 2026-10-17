//! MQTT adapter for payment authorization and durable approval notifications.
//!
//! The broker ACL is part of the authentication boundary: a workload may
//! publish only to its own client-scoped command topic, while an approver may
//! publish only to its own client-and-principal-scoped decision topic. This
//! adapter derives both identities from the topic and never accepts a client
//! or principal claim from the message body.

use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    net::IpAddr,
    str::FromStr as _,
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{anyhow, Context as _};
use async_trait::async_trait;
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rumqttc::{AsyncClient, Event, Incoming, MqttOptions, Publish, QoS, SubAck, SubscribeReasonCode, Transport};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tokio::{
    sync::{Mutex, OwnedSemaphorePermit, Semaphore},
    task::JoinSet,
};
use zeroize::Zeroizing;

use crate::{
    cache::ICache,
    cmd::{RuntimeComponent, Shutdown},
    config::MqttTransportConfig,
    controller::http::{
        AuthorizePaymentRequest, CreateChallengeRequest, PaymentApi, ReportOutboundResponseRequest,
        ResolveOfferRequest, SettlePaymentRequest, SubmitApprovalDecisionRequest, VerifyPaymentRequest,
    },
    handler::job::{PaymentEventDelivery, PaymentEventDispatcher, PaymentEventDispatcherConfig, PaymentEventSink},
    storage::{ClientStorage, OutboxDeliveryClass, OutboxLeaseRequest, OutboxMessage, OutboxStorage, ScenarioStorage},
    utils::secret::read_secret_ref,
    Error,
};

const AUTHORIZE_PATH: &str = "/api/v1/payments/authorize";
const COMMAND_SUFFIX: &str = "/commands/authorize";
const OUTBOUND_RESPONSE_SUFFIX: &str = "/commands/outbound-response";
const RESOLVE_OFFER_SUFFIX: &str = "/commands/resolve-offer";
const CREATE_CHALLENGE_SUFFIX: &str = "/commands/create-challenge";
const VERIFY_PAYMENT_SUFFIX: &str = "/commands/verify-payment";
const SETTLE_PAYMENT_SUFFIX: &str = "/commands/settle-payment";
const APPROVER_SEGMENT: &str = "approvers";
const DECISION_SEGMENT: &str = "decisions";
const MAX_ENVELOPE_BYTES: usize = 2 * 1024 * 1024;
const MAX_BODY_BYTES: usize = 1024 * 1024;
const MAX_HEADER_COUNT: usize = 16;
const MAX_HEADER_NAME_BYTES: usize = 64;
const MAX_HEADER_VALUE_BYTES: usize = 8 * 1024;
const MAX_IN_FLIGHT: usize = 128;
const MAX_IN_FLIGHT_PER_CLIENT: usize = 16;
const RESPONSE_CACHE_TTL: Duration = Duration::from_secs(5 * 60);
const OUTBOX_BATCH_SIZE: u32 = 64;
const OUTBOX_LEASE_TTL: Duration = Duration::from_secs(30);
const OUTBOX_POLL_INTERVAL: Duration = Duration::from_millis(250);
const MAX_SECRET_BYTES: usize = 16 * 1024;

/// Long-running MQTT transport. The contained password is resolved once from
/// an indirect secret reference and is redacted from `Debug` output.
pub struct MqttController {
    settings: MqttSettings,
    api: Arc<PaymentApi>,
    clients: Arc<dyn ClientStorage>,
    scenarios: Arc<dyn ScenarioStorage>,
    outbox: Arc<dyn OutboxStorage>,
    limiter: Arc<ConcurrencyLimiter>,
    responses: Arc<ResponseCache>,
}

impl MqttController {
    /// Resolves broker credentials and builds the transport component.
    ///
    /// # Errors
    ///
    /// Returns an error when the enabled MQTT configuration or its indirect
    /// password source is invalid.
    pub fn new(
        config: MqttTransportConfig,
        api: Arc<PaymentApi>,
        clients: Arc<dyn ClientStorage>,
        scenarios: Arc<dyn ScenarioStorage>,
        outbox: Arc<dyn OutboxStorage>,
        cache: Arc<dyn ICache>,
    ) -> crate::Result<Self> {
        validate_config(&config)?;
        let password = read_secret_ref(&config.password, "MQTT broker password", MAX_SECRET_BYTES)?;
        if password.is_empty() {
            return Err(Error::Config("MQTT broker password must not be empty".to_owned()));
        }
        let client_id = crate::utils::runtime_identity::unique_runtime_id(&config.client_id);
        Ok(Self {
            settings: MqttSettings {
                host: config.host,
                port: config.port,
                client_id,
                consumer_group: config.consumer_group,
                topic_prefix: config.topic_prefix,
                tls: config.tls,
                username: config.username,
                password,
                request_timeout: config.request_timeout,
                reconnect_delay: config.reconnect_delay,
            },
            api,
            clients,
            scenarios,
            outbox,
            limiter: Arc::new(ConcurrencyLimiter::new()),
            responses: Arc::new(ResponseCache::new(cache)),
        })
    }

    async fn serve(self, mut shutdown: Shutdown) -> anyhow::Result<()> {
        let (client, mut event_loop) = AsyncClient::new(mqtt_options(&self.settings), MAX_IN_FLIGHT * 2);
        let command_filters = command_subscription_filters(&self.settings.topic_prefix, &self.settings.consumer_group);
        let decision_filter = shared_subscription_filter(
            &self.settings.consumer_group,
            &format!("{}/+/approvers/+/decisions", self.settings.topic_prefix),
        );
        let mut outbox_shutdown = shutdown.clone();
        let outbox_task = tokio::spawn(run_outbox_publisher(
            client.clone(),
            Arc::clone(&self.clients),
            Arc::clone(&self.outbox),
            self.settings.clone_for_task(),
            outbox_shutdown.clone(),
        ));
        let payment_event_dispatcher = PaymentEventDispatcher::new(
            Arc::clone(&self.outbox),
            Arc::new(MqttPaymentEventSink {
                client: client.clone(),
                clients: Arc::clone(&self.clients),
                scenarios: Arc::clone(&self.scenarios),
                topic_prefix: self.settings.topic_prefix.clone(),
            }),
            self.settings.payment_event_dispatcher_config(),
        )
        .map_err(|error| anyhow!(error))?;
        let mut payment_event_shutdown = shutdown.clone();
        let payment_event_task = tokio::spawn(async move {
            payment_event_dispatcher
                .run_until(payment_event_shutdown.cancelled())
                .await
                .map_err(|error| anyhow!(error))
        });
        let mut workers = JoinSet::new();

        loop {
            tokio::select! {
                () = shutdown.cancelled() => break,
                event = event_loop.poll() => {
                    match event {
                        Ok(Event::Incoming(Incoming::ConnAck(_))) => {
                            self.subscribe_commands(&client, &command_filters, &decision_filter).await?;
                        }
                        Ok(Event::Incoming(Incoming::SubAck(ack))) => {
                            validate_subscription_ack(&ack, command_filters.len() + 1)?;
                            tracing::info!(
                                mqtt.client_id = %self.settings.client_id,
                                mqtt.consumer_group = %self.settings.consumer_group,
                                "AgentVault MQTT command subscriptions authorized"
                            );
                        }
                        Ok(Event::Incoming(Incoming::Publish(message))) => {
                            self.handle_command_publish(&client, &mut workers, message).await;
                        }
                        Ok(_) => {}
                        Err(error) => {
                            tracing::warn!(error = %error, "MQTT connection interrupted; retrying stable session");
                            tokio::select! {
                                () = shutdown.cancelled() => break,
                                () = tokio::time::sleep(self.settings.reconnect_delay) => {}
                            }
                        }
                    }
                }
                Some(result) = workers.join_next(), if !workers.is_empty() => {
                    if let Err(error) = result {
                        tracing::error!(error = %error, "MQTT request worker failed");
                    }
                }
            }
        }

        // `Shutdown` is clone-based; make the publisher observe cancellation
        // even when this component exits after its own receiver wakes.
        outbox_shutdown.cancelled().await;
        workers.abort_all();
        while workers.join_next().await.is_some() {}
        outbox_task.await.context("join MQTT outbox publisher")??;
        payment_event_task
            .await
            .context("join MQTT payment event dispatcher")??;
        Ok(())
    }

    async fn subscribe_commands(
        &self,
        client: &AsyncClient,
        command_filters: &[String],
        decision_filter: &str,
    ) -> anyhow::Result<()> {
        let filters = command_filters
            .iter()
            .cloned()
            .chain(std::iter::once(decision_filter.to_owned()))
            .map(|filter| rumqttc::SubscribeFilter::new(filter, QoS::AtLeastOnce));
        client
            .subscribe_many(filters)
            .await
            .context("subscribe to MQTT payment commands")?;
        tracing::debug!(
            mqtt.command_filters = ?command_filters,
            mqtt.decision_filter = %decision_filter,
            "requested AgentVault MQTT command subscriptions"
        );
        Ok(())
    }

    async fn handle_command_publish(&self, client: &AsyncClient, workers: &mut JoinSet<()>, message: Publish) {
        if message.qos != QoS::AtLeastOnce || message.retain {
            tracing::warn!(mqtt.topic = %message.topic, "rejected MQTT command without strict QoS 1 semantics");
            return;
        }
        if message.payload.len() > MAX_ENVELOPE_BYTES {
            tracing::warn!(mqtt.topic = %message.topic, "rejected oversized MQTT command");
            return;
        }
        let Some(command) = command_topic(&self.settings.topic_prefix, &message.topic) else {
            tracing::warn!(mqtt.topic = %message.topic, "rejected MQTT command on an invalid topic");
            return;
        };
        let client_id = command.client_id().to_owned();
        let approver_principal = command.approver_principal().map(str::to_owned);
        let kind = command.kind();
        let cache_scope = command.cache_scope();
        let Some(correlation_id) = probe_correlation_id(&message.payload) else {
            tracing::warn!(mqtt.topic = %message.topic, "rejected MQTT command without a valid correlation id");
            return;
        };
        let response_topic = command.response_topic(&self.settings.topic_prefix, &correlation_id);
        match self
            .responses
            .begin(&cache_scope, &correlation_id, digest(&message.payload))
            .await
        {
            CacheDisposition::Cached(response) => {
                spawn_publish(
                    workers,
                    client.clone(),
                    response_topic,
                    response,
                    self.settings.request_timeout,
                );
            }
            CacheDisposition::InFlight => {
                tracing::debug!(mqtt.client_id = %client_id, mqtt.correlation_id = %correlation_id, "coalesced duplicate MQTT command");
            }
            CacheDisposition::Conflict => {
                let response = error_response(
                    &correlation_id,
                    409,
                    "PAYMENT_CONFLICT",
                    "correlation id was reused with different content",
                );
                spawn_publish(
                    workers,
                    client.clone(),
                    response_topic,
                    response,
                    self.settings.request_timeout,
                );
            }
            CacheDisposition::New => {
                let work = CommandWork {
                    kind,
                    cache_scope,
                    client_id,
                    approver_principal,
                    correlation_id,
                    payload: message.payload.to_vec(),
                    response_topic,
                };
                self.spawn_new_command(client, workers, work).await;
            }
        }
    }

    async fn spawn_new_command(&self, client: &AsyncClient, workers: &mut JoinSet<()>, work: CommandWork) {
        let Some(permits) = self.limiter.try_acquire(&work.client_id).await else {
            let response = error_response(
                &work.correlation_id,
                429,
                "RATE_LIMITED",
                "too many concurrent MQTT payment requests",
            );
            self.responses
                .complete(&work.cache_scope, &work.correlation_id, response.clone())
                .await;
            spawn_publish(
                workers,
                client.clone(),
                work.response_topic,
                response,
                self.settings.request_timeout,
            );
            return;
        };
        let dependencies = CommandDependencies {
            client: client.clone(),
            api: Arc::clone(&self.api),
            clients: Arc::clone(&self.clients),
            scenarios: Arc::clone(&self.scenarios),
            responses: Arc::clone(&self.responses),
            timeout: self.settings.request_timeout,
        };
        workers.spawn(async move {
            let _permits = permits;
            process_command(work, dependencies).await;
        });
    }
}

pub(super) fn command_subscription_filters(prefix: &str, consumer_group: &str) -> [String; 6] {
    [
        shared_subscription_filter(consumer_group, &format!("{prefix}/+/commands/authorize")),
        shared_subscription_filter(consumer_group, &format!("{prefix}/+/commands/outbound-response")),
        shared_subscription_filter(consumer_group, &format!("{prefix}/+/commands/resolve-offer")),
        shared_subscription_filter(consumer_group, &format!("{prefix}/+/commands/create-challenge")),
        shared_subscription_filter(consumer_group, &format!("{prefix}/+/commands/verify-payment")),
        shared_subscription_filter(consumer_group, &format!("{prefix}/+/commands/settle-payment")),
    ]
}

fn shared_subscription_filter(consumer_group: &str, filter: &str) -> String {
    format!("$share/{consumer_group}/{filter}")
}

fn validate_subscription_ack(ack: &SubAck, expected: usize) -> anyhow::Result<()> {
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

fn mqtt_options(settings: &MqttSettings) -> MqttOptions {
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

impl fmt::Debug for MqttController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MqttController")
            .field("settings", &self.settings)
            .field("api", &"<payment-api>")
            .field("clients", &"<client-storage>")
            .field("scenarios", &"<scenario-storage>")
            .field("outbox", &"<outbox-storage>")
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl RuntimeComponent for MqttController {
    fn name(&self) -> &'static str {
        "MQTT payment controller"
    }

    async fn run(self: Box<Self>, shutdown: Shutdown) -> anyhow::Result<()> {
        self.serve(shutdown).await
    }
}

struct CommandWork {
    kind: CommandKind,
    cache_scope: String,
    client_id: String,
    approver_principal: Option<String>,
    correlation_id: String,
    payload: Vec<u8>,
    response_topic: String,
}

struct CommandDependencies {
    client: AsyncClient,
    api: Arc<PaymentApi>,
    clients: Arc<dyn ClientStorage>,
    scenarios: Arc<dyn ScenarioStorage>,
    responses: Arc<ResponseCache>,
    timeout: Duration,
}

async fn process_command(work: CommandWork, dependencies: CommandDependencies) {
    let response = match work.kind {
        CommandKind::Authorize => process_authorize_command(&work, &dependencies).await,
        CommandKind::OutboundResponse => process_outbound_response(&work, &dependencies).await,
        CommandKind::ResolveOffer => process_resolve_offer(&work, &dependencies).await,
        CommandKind::CreateChallenge => process_create_challenge(&work, &dependencies).await,
        CommandKind::VerifyPayment => process_verify_payment(&work, &dependencies).await,
        CommandKind::SettlePayment => process_settle_payment(&work, &dependencies).await,
        CommandKind::ApprovalDecision => process_approval_decision(&work, &dependencies).await,
    };
    dependencies
        .responses
        .complete(&work.cache_scope, &work.correlation_id, response.clone())
        .await;
    if let Err(error) = publish_response(
        &dependencies.client,
        &work.response_topic,
        &response,
        dependencies.timeout,
    )
    .await
    {
        tracing::warn!(
            error = %error,
            mqtt.client_id = %work.client_id,
            mqtt.correlation_id = %work.correlation_id,
            "failed to publish MQTT authorization response"
        );
    }
}

async fn process_authorize_command(work: &CommandWork, dependencies: &CommandDependencies) -> ResponseEnvelope {
    let command = match parse_authorize_command(&work.payload) {
        Ok(command) => command,
        Err(error) => return error_response(&work.correlation_id, 400, "INVALID_REQUEST", error),
    };
    match tokio::time::timeout(
        dependencies.timeout,
        authorize_for_topic_client(
            dependencies.api.as_ref(),
            dependencies.clients.as_ref(),
            &work.client_id,
            &command,
        ),
    )
    .await
    {
        Ok(Ok(value)) => success_response("payment.authorization_result", &work.correlation_id, &value),
        Ok(Err(error)) => response_for_error(&work.correlation_id, error),
        Err(_) => error_response(
            &work.correlation_id,
            408,
            "REQUEST_TIMEOUT",
            "payment authorization timed out",
        ),
    }
}

async fn process_outbound_response(work: &CommandWork, dependencies: &CommandDependencies) -> ResponseEnvelope {
    let command = match parse_outbound_response_command(&work.payload) {
        Ok(command) => command,
        Err(error) => return error_response(&work.correlation_id, 400, "INVALID_REQUEST", error),
    };
    match tokio::time::timeout(
        dependencies.timeout,
        report_outbound_response_for_topic_client(
            dependencies.api.as_ref(),
            dependencies.clients.as_ref(),
            &work.client_id,
            &command,
        ),
    )
    .await
    {
        Ok(Ok(value)) => success_response("payment.outbound_response_result", &work.correlation_id, &value),
        Ok(Err(error)) => response_for_error(&work.correlation_id, error),
        Err(_) => error_response(
            &work.correlation_id,
            408,
            "REQUEST_TIMEOUT",
            "outbound payment evidence reporting timed out",
        ),
    }
}

async fn process_resolve_offer(work: &CommandWork, dependencies: &CommandDependencies) -> ResponseEnvelope {
    let request = match parse_payee_command::<ResolveOfferRequest>(
        &work.payload,
        "payment.resolve_offer",
        "/api/v1/offers/resolve",
    ) {
        Ok(request) => request,
        Err(error) => return error_response(&work.correlation_id, 400, "INVALID_REQUEST", error),
    };
    let result = async {
        let principal = principal_for_topic_client(dependencies.clients.as_ref(), &work.client_id).await?;
        dependencies.api.resolve_offer(&principal, &request).await
    };
    timed_payment_response(
        dependencies.timeout,
        result,
        "payment.offer_resolved",
        &work.correlation_id,
        "offer resolution timed out",
    )
    .await
}

async fn process_create_challenge(work: &CommandWork, dependencies: &CommandDependencies) -> ResponseEnvelope {
    let request = match parse_payee_command::<CreateChallengeRequest>(
        &work.payload,
        "payment.create_challenge",
        "/api/v1/payee/challenges",
    ) {
        Ok(request) => request,
        Err(error) => return error_response(&work.correlation_id, 400, "INVALID_REQUEST", error),
    };
    let result = async {
        let principal = principal_for_topic_client(dependencies.clients.as_ref(), &work.client_id).await?;
        dependencies
            .api
            .create_challenge(&principal, &request, unix_time()?)
            .await
    };
    timed_payment_response(
        dependencies.timeout,
        result,
        "payment.challenge_created",
        &work.correlation_id,
        "challenge creation timed out",
    )
    .await
}

async fn process_verify_payment(work: &CommandWork, dependencies: &CommandDependencies) -> ResponseEnvelope {
    let request =
        match parse_payee_command::<VerifyPaymentRequest>(&work.payload, "payment.verify", "/api/v1/payee/verify") {
            Ok(request) => request,
            Err(error) => return error_response(&work.correlation_id, 400, "INVALID_REQUEST", error),
        };
    let result = async {
        let principal = principal_for_topic_client(dependencies.clients.as_ref(), &work.client_id).await?;
        dependencies
            .api
            .verify_payment(&principal, &request, unix_time()?)
            .await
    };
    timed_payment_response(
        dependencies.timeout,
        result,
        "payment.verification_result",
        &work.correlation_id,
        "payment verification timed out",
    )
    .await
}

async fn process_settle_payment(work: &CommandWork, dependencies: &CommandDependencies) -> ResponseEnvelope {
    let request =
        match parse_payee_command::<SettlePaymentRequest>(&work.payload, "payment.settle", "/api/v1/payee/settle") {
            Ok(request) => request,
            Err(error) => return error_response(&work.correlation_id, 400, "INVALID_REQUEST", error),
        };
    let result = async {
        let principal = principal_for_topic_client(dependencies.clients.as_ref(), &work.client_id).await?;
        dependencies
            .api
            .settle_payment(&principal, &request, unix_time()?)
            .await
    };
    timed_payment_response(
        dependencies.timeout,
        result,
        "payment.settlement_result",
        &work.correlation_id,
        "payment settlement timed out",
    )
    .await
}

async fn timed_payment_response<T, F>(
    timeout: Duration,
    operation: F,
    response_type: &'static str,
    correlation_id: &str,
    timeout_message: &'static str,
) -> ResponseEnvelope
where
    T: Serialize,
    F: std::future::Future<Output = crate::Result<T>>,
{
    match tokio::time::timeout(timeout, operation).await {
        Ok(Ok(value)) => success_response(response_type, correlation_id, &value),
        Ok(Err(error)) => response_for_error(correlation_id, error),
        Err(_) => error_response(correlation_id, 408, "REQUEST_TIMEOUT", timeout_message),
    }
}

async fn principal_for_topic_client(clients: &dyn ClientStorage, client_id: &str) -> crate::Result<String> {
    let client = clients
        .get(client_id)
        .await?
        .filter(|client| client.enabled)
        .ok_or(Error::Forbidden)?;
    if client.id != client_id || client.auth_principal.trim().is_empty() {
        return Err(Error::Forbidden);
    }
    Ok(client.auth_principal)
}

async fn process_approval_decision(work: &CommandWork, dependencies: &CommandDependencies) -> ResponseEnvelope {
    let command = match parse_decision_command(&work.payload) {
        Ok(command) => command,
        Err(error) => return error_response(&work.correlation_id, error.status, error.code, error.message),
    };
    match tokio::time::timeout(
        dependencies.timeout,
        decide_for_topic_client(
            dependencies.api.as_ref(),
            dependencies.clients.as_ref(),
            dependencies.scenarios.as_ref(),
            &work.client_id,
            work.approver_principal.as_deref(),
            &command,
        ),
    )
    .await
    {
        Ok(Ok(value)) => success_response("payment.approval_decision_result", &work.correlation_id, &value),
        Ok(Err(error)) => response_for_error(&work.correlation_id, error),
        Err(_) => error_response(
            &work.correlation_id,
            408,
            "REQUEST_TIMEOUT",
            "approval decision timed out",
        ),
    }
}

async fn authorize_for_topic_client(
    api: &PaymentApi,
    clients: &dyn ClientStorage,
    client_id: &str,
    request: &AuthorizePaymentRequest,
) -> crate::Result<crate::controller::http::AuthorizePaymentResponse> {
    let client = clients
        .get(client_id)
        .await?
        .filter(|client| client.enabled)
        .ok_or(Error::Forbidden)?;
    if client.id != client_id || client.auth_principal.trim().is_empty() {
        return Err(Error::Forbidden);
    }
    api.authorize(&client.auth_principal, request, unix_time()?).await
}

struct OutboundResponseCommand {
    intent_id: String,
    request: ReportOutboundResponseRequest,
}

async fn report_outbound_response_for_topic_client(
    api: &PaymentApi,
    clients: &dyn ClientStorage,
    client_id: &str,
    command: &OutboundResponseCommand,
) -> crate::Result<crate::controller::http::ReportOutboundResponseResponse> {
    let client = clients
        .get(client_id)
        .await?
        .filter(|client| client.enabled)
        .ok_or(Error::Forbidden)?;
    if client.id != client_id || client.auth_principal.trim().is_empty() {
        return Err(Error::Forbidden);
    }
    api.report_outbound_response(
        &client.auth_principal,
        &command.intent_id,
        &command.request,
        unix_time()?,
    )
    .await
}

#[derive(Debug)]
struct ApprovalDecisionCommand {
    request: SubmitApprovalDecisionRequest,
}

async fn decide_for_topic_client(
    api: &PaymentApi,
    clients: &dyn ClientStorage,
    scenarios: &dyn ScenarioStorage,
    client_id: &str,
    approver_principal: Option<&str>,
    command: &ApprovalDecisionCommand,
) -> crate::Result<crate::model::ApprovalRequest> {
    let approver_principal = approver_principal.ok_or(Error::Forbidden)?;
    let client = clients
        .get(client_id)
        .await?
        .filter(|client| client.enabled)
        .ok_or(Error::Forbidden)?;
    if client.id != client_id || client.auth_principal.trim().is_empty() {
        return Err(Error::Forbidden);
    }
    let scenario = scenarios
        .get_for_client(client_id, &command.request.context.scenario_id)
        .await?
        .filter(|scenario| scenario.enabled)
        .ok_or(Error::Forbidden)?;
    if !scenario.belongs_to(client_id) {
        return Err(Error::Forbidden);
    }
    api.decide_approval(approver_principal, &command.request, unix_time()?)
        .await
}

fn spawn_publish(
    workers: &mut JoinSet<()>,
    client: AsyncClient,
    topic: String,
    response: ResponseEnvelope,
    timeout: Duration,
) {
    workers.spawn(async move {
        if let Err(error) = publish_response(&client, &topic, &response, timeout).await {
            tracing::warn!(error = %error, mqtt.topic = %topic, "failed to republish MQTT authorization response");
        }
    });
}

async fn publish_response(
    client: &AsyncClient,
    topic: &str,
    response: &ResponseEnvelope,
    timeout: Duration,
) -> anyhow::Result<()> {
    let payload = serde_json::to_vec(response).context("encode MQTT authorization response")?;
    if payload.len() > MAX_ENVELOPE_BYTES {
        return Err(anyhow!("MQTT authorization response exceeds the wire limit"));
    }
    tokio::time::timeout(timeout, client.publish(topic, QoS::AtLeastOnce, false, payload))
        .await
        .context("queue MQTT authorization response before timeout")?
        .context("queue MQTT authorization response")
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestEnvelope {
    #[serde(rename = "type")]
    message_type: String,
    version: u8,
    correlation_id: String,
    request: WireRequest,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
    body: String,
}

fn parse_authorize_command(payload: &[u8]) -> Result<AuthorizePaymentRequest, &'static str> {
    if payload.len() > MAX_ENVELOPE_BYTES {
        return Err("MQTT authorization envelope is too large");
    }
    let envelope: RequestEnvelope =
        serde_json::from_slice(payload).map_err(|_| "invalid MQTT authorization envelope")?;
    if envelope.message_type != "payment.authorize"
        || envelope.version != 1
        || !valid_correlation_id(&envelope.correlation_id)
        || envelope.request.method != "POST"
        || envelope.request.path != AUTHORIZE_PATH
    {
        return Err("invalid MQTT authorization envelope contract");
    }
    validate_authorize_headers(&envelope.request.headers)?;
    let body = decode_canonical_body(&envelope.request.body)?;
    let request: AuthorizePaymentRequest =
        serde_json::from_slice(&body).map_err(|_| "invalid MQTT authorization request body")?;
    Ok(request)
}

fn parse_outbound_response_command(payload: &[u8]) -> Result<OutboundResponseCommand, &'static str> {
    if payload.len() > MAX_ENVELOPE_BYTES {
        return Err("MQTT outbound response envelope is too large");
    }
    let envelope: RequestEnvelope =
        serde_json::from_slice(payload).map_err(|_| "invalid MQTT outbound response envelope")?;
    let intent_id = outbound_intent_id_from_path(&envelope.request.path)
        .ok_or("invalid MQTT outbound response path")?
        .to_owned();
    if envelope.message_type != "payment.outbound_response"
        || envelope.version != 1
        || !valid_correlation_id(&envelope.correlation_id)
        || envelope.request.method != "POST"
    {
        return Err("invalid MQTT outbound response envelope contract");
    }
    validate_authorize_headers(&envelope.request.headers)?;
    let body = decode_canonical_body(&envelope.request.body)?;
    let request: ReportOutboundResponseRequest =
        serde_json::from_slice(&body).map_err(|_| "invalid MQTT outbound response body")?;
    Ok(OutboundResponseCommand { intent_id, request })
}

fn parse_payee_command<T>(payload: &[u8], message_type: &str, path: &str) -> Result<T, &'static str>
where
    T: serde::de::DeserializeOwned,
{
    if payload.len() > MAX_ENVELOPE_BYTES {
        return Err("MQTT payee envelope is too large");
    }
    let envelope: RequestEnvelope = serde_json::from_slice(payload).map_err(|_| "invalid MQTT payee envelope")?;
    if envelope.message_type != message_type
        || envelope.version != 1
        || !valid_correlation_id(&envelope.correlation_id)
        || envelope.request.method != "POST"
        || envelope.request.path != path
    {
        return Err("invalid MQTT payee envelope contract");
    }
    validate_wire_headers(&envelope.request.headers)?;
    let body = decode_canonical_body(&envelope.request.body)?;
    serde_json::from_slice(&body).map_err(|_| "invalid MQTT payee request body")
}

fn outbound_intent_id_from_path(path: &str) -> Option<&str> {
    let intent_id = path
        .strip_prefix("/api/v1/payments/")?
        .strip_suffix("/outbound-response")?;
    (!intent_id.contains('/') && valid_identifier(intent_id)).then_some(intent_id)
}

#[derive(Debug)]
struct DecisionParseError {
    status: u16,
    code: &'static str,
    message: &'static str,
}

impl DecisionParseError {
    const fn invalid(message: &'static str) -> Self {
        Self {
            status: 400,
            code: "INVALID_REQUEST",
            message,
        }
    }
}

fn parse_decision_command(payload: &[u8]) -> std::result::Result<ApprovalDecisionCommand, DecisionParseError> {
    if payload.len() > MAX_ENVELOPE_BYTES {
        return Err(DecisionParseError::invalid("MQTT approval envelope is too large"));
    }
    let envelope: RequestEnvelope =
        serde_json::from_slice(payload).map_err(|_| DecisionParseError::invalid("invalid MQTT approval envelope"))?;
    if envelope.message_type != "payment.approval_decision"
        || envelope.version != 1
        || !valid_correlation_id(&envelope.correlation_id)
        || envelope.request.method != "POST"
    {
        return Err(DecisionParseError::invalid("invalid MQTT approval envelope contract"));
    }
    let path_approval_id = approval_id_from_decision_path(&envelope.request.path)
        .ok_or_else(|| DecisionParseError::invalid("invalid MQTT approval decision path"))?;
    validate_decision_headers(&envelope.request.headers).map_err(DecisionParseError::invalid)?;
    let body = decode_canonical_body(&envelope.request.body).map_err(DecisionParseError::invalid)?;
    let request: SubmitApprovalDecisionRequest = serde_json::from_slice(&body)
        .map_err(|_| DecisionParseError::invalid("invalid MQTT approval decision body"))?;
    if path_approval_id != request.approval_id {
        return Err(DecisionParseError::invalid("approval path and body binding mismatch"));
    }
    Ok(ApprovalDecisionCommand { request })
}

fn approval_id_from_decision_path(path: &str) -> Option<&str> {
    let value = path.strip_prefix("/api/v1/approvals/")?.strip_suffix("/decision")?;
    (!value.is_empty()
        && !value.contains('/')
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')))
    .then_some(value)
}

fn validate_authorize_headers(headers: &BTreeMap<String, String>) -> Result<(), &'static str> {
    validate_wire_headers(headers)
}

fn validate_decision_headers(headers: &BTreeMap<String, String>) -> Result<(), &'static str> {
    validate_wire_headers(headers)
}

fn validate_wire_headers(headers: &BTreeMap<String, String>) -> Result<(), &'static str> {
    if headers.is_empty() || headers.len() > MAX_HEADER_COUNT {
        return Err("invalid MQTT authorization headers");
    }
    for (name, value) in headers {
        if name.is_empty()
            || name.len() > MAX_HEADER_NAME_BYTES
            || value.len() > MAX_HEADER_VALUE_BYTES
            || !name.bytes().all(|byte| byte.is_ascii_lowercase() || byte == b'-')
            || value.bytes().any(|byte| byte == b'\r' || byte == b'\n' || byte == 0)
            || !matches!(name.as_str(), "accept" | "content-type")
        {
            return Err("invalid MQTT authorization headers");
        }
    }
    if headers.get("content-type").map(String::as_str) != Some("application/json") {
        return Err("content-type must be application/json");
    }
    if headers.get("accept").is_some_and(|value| value != "application/json") {
        return Err("accept must be application/json");
    }
    Ok(())
}

fn decode_canonical_body(encoded: &str) -> Result<Vec<u8>, &'static str> {
    let max_encoded = MAX_BODY_BYTES.saturating_mul(4).div_ceil(3);
    if encoded.len() > max_encoded || !encoded.bytes().all(is_base64url_byte) {
        return Err("invalid or oversized MQTT authorization body");
    }
    let decoded = URL_SAFE_NO_PAD
        .decode(encoded)
        .map_err(|_| "invalid MQTT authorization body encoding")?;
    if decoded.len() > MAX_BODY_BYTES || URL_SAFE_NO_PAD.encode(&decoded) != encoded {
        return Err("non-canonical or oversized MQTT authorization body");
    }
    Ok(decoded)
}

const fn is_base64url_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_'
}

fn probe_correlation_id(payload: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(payload).ok()?;
    let correlation_id = value.get("correlationId")?.as_str()?;
    valid_correlation_id(correlation_id).then(|| correlation_id.to_owned())
}

fn valid_correlation_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

#[derive(Clone, Copy)]
enum CommandKind {
    Authorize,
    OutboundResponse,
    ResolveOffer,
    CreateChallenge,
    VerifyPayment,
    SettlePayment,
    ApprovalDecision,
}

impl CommandKind {
    const fn cache_scope(self) -> &'static str {
        match self {
            Self::Authorize => "authorize",
            Self::OutboundResponse => "outbound-response",
            Self::ResolveOffer => "resolve-offer",
            Self::CreateChallenge => "create-challenge",
            Self::VerifyPayment => "verify-payment",
            Self::SettlePayment => "settle-payment",
            Self::ApprovalDecision => "approval-decision",
        }
    }
}

enum CommandTopic {
    Authorize(String),
    OutboundResponse(String),
    ResolveOffer(String),
    CreateChallenge(String),
    VerifyPayment(String),
    SettlePayment(String),
    ApprovalDecision {
        client_id: String,
        approver_principal: String,
    },
}

impl CommandTopic {
    fn client_id(&self) -> &str {
        match self {
            Self::Authorize(client_id)
            | Self::OutboundResponse(client_id)
            | Self::ResolveOffer(client_id)
            | Self::CreateChallenge(client_id)
            | Self::VerifyPayment(client_id)
            | Self::SettlePayment(client_id)
            | Self::ApprovalDecision { client_id, .. } => client_id,
        }
    }

    fn approver_principal(&self) -> Option<&str> {
        match self {
            Self::ApprovalDecision { approver_principal, .. } => Some(approver_principal),
            Self::Authorize(_)
            | Self::OutboundResponse(_)
            | Self::ResolveOffer(_)
            | Self::CreateChallenge(_)
            | Self::VerifyPayment(_)
            | Self::SettlePayment(_) => None,
        }
    }

    const fn kind(&self) -> CommandKind {
        match self {
            Self::Authorize(_) => CommandKind::Authorize,
            Self::OutboundResponse(_) => CommandKind::OutboundResponse,
            Self::ResolveOffer(_) => CommandKind::ResolveOffer,
            Self::CreateChallenge(_) => CommandKind::CreateChallenge,
            Self::VerifyPayment(_) => CommandKind::VerifyPayment,
            Self::SettlePayment(_) => CommandKind::SettlePayment,
            Self::ApprovalDecision { .. } => CommandKind::ApprovalDecision,
        }
    }

    fn cache_scope(&self) -> String {
        match self {
            Self::ApprovalDecision {
                client_id,
                approver_principal,
            } => format!(
                "{client_id}:{}:{approver_principal}",
                CommandKind::ApprovalDecision.cache_scope()
            ),
            Self::Authorize(client_id) => {
                format!("{client_id}:{}", CommandKind::Authorize.cache_scope())
            }
            Self::OutboundResponse(client_id) => {
                format!("{client_id}:{}", CommandKind::OutboundResponse.cache_scope())
            }
            Self::ResolveOffer(client_id) => {
                format!("{client_id}:{}", CommandKind::ResolveOffer.cache_scope())
            }
            Self::CreateChallenge(client_id) => {
                format!("{client_id}:{}", CommandKind::CreateChallenge.cache_scope())
            }
            Self::VerifyPayment(client_id) => {
                format!("{client_id}:{}", CommandKind::VerifyPayment.cache_scope())
            }
            Self::SettlePayment(client_id) => {
                format!("{client_id}:{}", CommandKind::SettlePayment.cache_scope())
            }
        }
    }

    fn response_topic(&self, prefix: &str, correlation_id: &str) -> String {
        match self {
            Self::Authorize(client_id)
            | Self::OutboundResponse(client_id)
            | Self::ResolveOffer(client_id)
            | Self::CreateChallenge(client_id)
            | Self::VerifyPayment(client_id)
            | Self::SettlePayment(client_id) => {
                format!("{prefix}/{client_id}/results/{correlation_id}")
            }
            Self::ApprovalDecision {
                client_id,
                approver_principal,
            } => format!("{prefix}/{client_id}/approvers/{approver_principal}/results/{correlation_id}"),
        }
    }
}

fn command_topic(prefix: &str, topic: &str) -> Option<CommandTopic> {
    let relative = topic.strip_prefix(prefix)?.strip_prefix('/')?;
    let segments = relative.split('/').collect::<Vec<_>>();
    if let [client_id, APPROVER_SEGMENT, approver_principal, DECISION_SEGMENT] = segments.as_slice() {
        return (valid_identifier(client_id) && valid_identifier(approver_principal)).then(|| {
            CommandTopic::ApprovalDecision {
                client_id: (*client_id).to_owned(),
                approver_principal: (*approver_principal).to_owned(),
            }
        });
    }
    for (suffix, constructor) in [
        (COMMAND_SUFFIX, CommandTopic::Authorize as fn(String) -> CommandTopic),
        (
            OUTBOUND_RESPONSE_SUFFIX,
            CommandTopic::OutboundResponse as fn(String) -> CommandTopic,
        ),
        (
            RESOLVE_OFFER_SUFFIX,
            CommandTopic::ResolveOffer as fn(String) -> CommandTopic,
        ),
        (
            CREATE_CHALLENGE_SUFFIX,
            CommandTopic::CreateChallenge as fn(String) -> CommandTopic,
        ),
        (
            VERIFY_PAYMENT_SUFFIX,
            CommandTopic::VerifyPayment as fn(String) -> CommandTopic,
        ),
        (
            SETTLE_PAYMENT_SUFFIX,
            CommandTopic::SettlePayment as fn(String) -> CommandTopic,
        ),
    ] {
        if let Some(client_id) = relative.strip_suffix(suffix) {
            return (!client_id.contains('/') && valid_identifier(client_id))
                .then(|| constructor(client_id.to_owned()));
        }
    }
    None
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct ResponseEnvelope {
    #[serde(rename = "type")]
    message_type: String,
    version: u8,
    correlation_id: String,
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

fn success_response(message_type: &'static str, correlation_id: &str, response: &impl Serialize) -> ResponseEnvelope {
    match serde_json::to_vec(response) {
        Ok(body) => encoded_response(message_type, correlation_id, 200, body),
        Err(_) => error_response(
            correlation_id,
            500,
            "INTERNAL_ERROR",
            "the operation could not be completed",
        ),
    }
}

fn response_for_error(correlation_id: &str, error: Error) -> ResponseEnvelope {
    let (status, code, message) = match error {
        Error::InvalidRequest(_) | Error::PaymentProtocol(_) => (400, "INVALID_REQUEST", error.to_string()),
        Error::Unauthenticated => (
            401,
            "UNAUTHORIZED",
            "a valid authenticated MQTT identity is required".to_owned(),
        ),
        Error::Forbidden | Error::PolicyDenied(_) => (403, "FORBIDDEN", error.to_string()),
        Error::NotFound(_) => (404, "NOT_FOUND", error.to_string()),
        Error::PaymentConflict(_) | Error::InvalidTransition(_) => (409, "PAYMENT_CONFLICT", error.to_string()),
        other => {
            tracing::error!(error = %other, "MQTT payment operation failed");
            (500, "INTERNAL_ERROR", "the operation could not be completed".to_owned())
        }
    };
    error_response(correlation_id, status, code, &message)
}

fn error_response(correlation_id: &str, status: u16, code: &'static str, message: &str) -> ResponseEnvelope {
    let body = serde_json::to_vec(&json!({"error": {"code": code, "message": message}})).unwrap_or_else(|_| {
        br#"{"error":{"code":"INTERNAL_ERROR","message":"the operation could not be completed"}}"#.to_vec()
    });
    encoded_response("payment.error", correlation_id, status, body)
}

fn encoded_response(message_type: &'static str, correlation_id: &str, status: u16, body: Vec<u8>) -> ResponseEnvelope {
    ResponseEnvelope {
        message_type: message_type.to_owned(),
        version: 1,
        correlation_id: correlation_id.to_owned(),
        status,
        headers: BTreeMap::from([("content-type".to_owned(), "application/json".to_owned())]),
        body: URL_SAFE_NO_PAD.encode(body),
    }
}

struct ConcurrencyPermits {
    _global: OwnedSemaphorePermit,
    _client: OwnedSemaphorePermit,
}

struct ConcurrencyLimiter {
    global: Arc<Semaphore>,
    clients: Mutex<HashMap<String, Arc<Semaphore>>>,
}

impl ConcurrencyLimiter {
    fn new() -> Self {
        Self {
            global: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            clients: Mutex::new(HashMap::new()),
        }
    }

    async fn try_acquire(&self, client_id: &str) -> Option<ConcurrencyPermits> {
        let global = Arc::clone(&self.global).try_acquire_owned().ok()?;
        let client_limiter = {
            let mut clients = self.clients.lock().await;
            Arc::clone(
                clients
                    .entry(client_id.to_owned())
                    .or_insert_with(|| Arc::new(Semaphore::new(MAX_IN_FLIGHT_PER_CLIENT))),
            )
        };
        let client = client_limiter.try_acquire_owned().ok()?;
        Some(ConcurrencyPermits {
            _global: global,
            _client: client,
        })
    }
}

enum CacheDisposition {
    New,
    InFlight,
    Cached(ResponseEnvelope),
    Conflict,
}

struct ResponseCache {
    cache: Arc<dyn ICache>,
}

#[derive(Deserialize, Serialize)]
struct CachedRequest {
    request_hash: [u8; 32],
    response: Option<ResponseEnvelope>,
}

impl ResponseCache {
    fn new(cache: Arc<dyn ICache>) -> Self {
        Self { cache }
    }

    async fn begin(&self, scope: &str, correlation_id: &str, request_hash: [u8; 32]) -> CacheDisposition {
        let key = response_cache_key(scope, correlation_id);
        let pending = CachedRequest {
            request_hash,
            response: None,
        };
        let Ok(encoded) = serde_json::to_vec(&pending) else {
            tracing::warn!("failed to encode MQTT response-cache entry; continuing without cache");
            return CacheDisposition::New;
        };
        match self
            .cache
            .set_if_absent("mqtt_response", &key, encoded, RESPONSE_CACHE_TTL)
            .await
        {
            Ok(true) => return CacheDisposition::New,
            Ok(false) => {}
            Err(error) => {
                tracing::warn!(%error, "MQTT response cache is unavailable; relying on SQL idempotency");
                return CacheDisposition::New;
            }
        }
        let existing = match self.cache.get("mqtt_response", &key).await {
            Ok(Some(existing)) => existing,
            Ok(None) => return CacheDisposition::New,
            Err(error) => {
                tracing::warn!(%error, "MQTT response cache is unavailable; relying on SQL idempotency");
                return CacheDisposition::New;
            }
        };
        let Ok(existing) = serde_json::from_slice::<CachedRequest>(&existing) else {
            tracing::warn!("discarding malformed non-authoritative MQTT response-cache entry");
            let _ = self.cache.delete("mqtt_response", &key).await;
            return CacheDisposition::New;
        };
        if existing.request_hash != request_hash {
            CacheDisposition::Conflict
        } else if let Some(response) = existing.response {
            CacheDisposition::Cached(response)
        } else {
            CacheDisposition::InFlight
        }
    }

    async fn complete(&self, scope: &str, correlation_id: &str, response: ResponseEnvelope) {
        let key = response_cache_key(scope, correlation_id);
        let existing = match self.cache.get("mqtt_response", &key).await {
            Ok(Some(existing)) => existing,
            Ok(None) => return,
            Err(error) => {
                tracing::warn!(%error, "MQTT response cache is unavailable; response remains durable through SQL");
                return;
            }
        };
        let Ok(mut entry) = serde_json::from_slice::<CachedRequest>(&existing) else {
            let _ = self.cache.delete("mqtt_response", &key).await;
            return;
        };
        entry.response = Some(response);
        let Ok(encoded) = serde_json::to_vec(&entry) else {
            return;
        };
        if let Err(error) = self.cache.set("mqtt_response", &key, encoded, RESPONSE_CACHE_TTL).await {
            tracing::warn!(%error, "MQTT response cache is unavailable; response remains durable through SQL");
        }
    }
}

fn response_cache_key(scope: &str, correlation_id: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(scope.as_bytes());
    digest.update([0]);
    digest.update(correlation_id.as_bytes());
    hex::encode(digest.finalize())
}

fn digest(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

#[derive(Clone)]
struct OutboxSettings {
    topic_prefix: String,
    lease_owner: String,
    request_timeout: Duration,
}

struct MqttPaymentEventSink {
    client: AsyncClient,
    clients: Arc<dyn ClientStorage>,
    scenarios: Arc<dyn ScenarioStorage>,
    topic_prefix: String,
}

enum PaymentEventDestination<'a> {
    Client(&'a str),
    Scenario(&'a str),
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PaymentEventEnvelope<'a> {
    #[serde(rename = "type")]
    message_type: &'static str,
    version: u8,
    correlation_id: &'a str,
    run_id: &'a str,
    aggregate_type: &'a str,
    aggregate_id: &'a str,
    event_type: &'a str,
    payload: &'a Value,
}

#[async_trait]
impl PaymentEventSink for MqttPaymentEventSink {
    async fn deliver(&self, message: &OutboxMessage) -> PaymentEventDelivery {
        let destination = match payment_event_destination(&message.destination) {
            Ok(destination) => destination,
            Err(reason) => return permanent_delivery_failure(reason),
        };
        let client_id = match self.resolve_client(destination).await {
            Ok(client_id) => client_id,
            Err(disposition) => return disposition,
        };
        if !valid_identifier(&message.event_type) {
            return permanent_delivery_failure("payment event type is not a safe MQTT topic segment");
        }
        let envelope = match payment_event_envelope(message) {
            Ok(envelope) => envelope,
            Err(reason) => return permanent_delivery_failure(reason),
        };
        let payload = match serde_json::to_vec(&envelope) {
            Ok(payload) if payload.len() <= MAX_ENVELOPE_BYTES => payload,
            Ok(_) => return permanent_delivery_failure("payment event exceeds the MQTT wire limit"),
            Err(_) => return permanent_delivery_failure("payment event cannot be encoded"),
        };
        let topic = format!(
            "{}/{client_id}/payments/events/{}",
            self.topic_prefix, message.event_type
        );
        match self.client.publish(topic, QoS::AtLeastOnce, false, payload).await {
            Ok(()) => PaymentEventDelivery::Delivered,
            Err(error) => PaymentEventDelivery::Retryable {
                reason: format!("MQTT QoS 1 payment event publish failed: {error}"),
            },
        }
    }
}

impl MqttPaymentEventSink {
    async fn resolve_client(&self, destination: PaymentEventDestination<'_>) -> Result<String, PaymentEventDelivery> {
        let client_id = match destination {
            PaymentEventDestination::Client(client_id) => client_id.to_owned(),
            PaymentEventDestination::Scenario(scenario_id) => match self.scenarios.get(scenario_id).await {
                Ok(Some(scenario)) if scenario.id == scenario_id && valid_identifier(&scenario.client_id) => {
                    scenario.client_id
                }
                Ok(Some(_)) => {
                    return Err(permanent_delivery_failure(
                        "payment event scenario ownership is invalid",
                    ));
                }
                Ok(None) => return Err(permanent_delivery_failure("payment event scenario does not exist")),
                Err(error) => {
                    return Err(retryable_delivery_failure(format!(
                        "resolve payment event scenario: {error}"
                    )));
                }
            },
        };
        match self.clients.get(&client_id).await {
            Ok(Some(client)) if client.id == client_id && client.enabled => Ok(client_id),
            Ok(Some(client)) if client.id == client_id => Err(retryable_delivery_failure(
                "payment event client is disabled".to_owned(),
            )),
            Ok(Some(_)) => Err(permanent_delivery_failure("payment event client ownership is invalid")),
            Ok(None) => Err(permanent_delivery_failure("payment event client does not exist")),
            Err(error) => Err(retryable_delivery_failure(format!(
                "resolve payment event client: {error}"
            ))),
        }
    }
}

fn payment_event_destination(destination: &str) -> Result<PaymentEventDestination<'_>, &'static str> {
    if let Some(identifier) = destination
        .strip_prefix("client:")
        .and_then(|value| value.strip_suffix(".payments"))
    {
        return valid_identifier(identifier)
            .then_some(PaymentEventDestination::Client(identifier))
            .ok_or("payment event destination identifier is invalid");
    }
    if let Some(identifier) = destination
        .strip_prefix("scenario:")
        .and_then(|value| value.strip_suffix(".payments"))
    {
        return valid_identifier(identifier)
            .then_some(PaymentEventDestination::Scenario(identifier))
            .ok_or("payment event destination identifier is invalid");
    }
    Err("payment event destination is invalid")
}

fn payment_event_envelope(message: &OutboxMessage) -> Result<PaymentEventEnvelope<'_>, &'static str> {
    for value in [
        message.correlation_id.as_str(),
        message.run_id.as_str(),
        message.aggregate_type.as_str(),
        message.aggregate_id.as_str(),
    ] {
        if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
            return Err("payment event envelope identity is invalid");
        }
    }
    Ok(PaymentEventEnvelope {
        message_type: "payment.event",
        version: 1,
        correlation_id: &message.correlation_id,
        run_id: &message.run_id,
        aggregate_type: &message.aggregate_type,
        aggregate_id: &message.aggregate_id,
        event_type: &message.event_type,
        payload: &message.payload,
    })
}

fn permanent_delivery_failure(reason: &'static str) -> PaymentEventDelivery {
    PaymentEventDelivery::PermanentFailure {
        reason: reason.to_owned(),
    }
}

fn retryable_delivery_failure(reason: String) -> PaymentEventDelivery {
    PaymentEventDelivery::Retryable { reason }
}

async fn run_outbox_publisher(
    client: AsyncClient,
    clients: Arc<dyn ClientStorage>,
    outbox: Arc<dyn OutboxStorage>,
    settings: OutboxSettings,
    mut shutdown: Shutdown,
) -> anyhow::Result<()> {
    loop {
        let now = unix_time().map_err(|error| anyhow!(error))?;
        let lease_until = now.saturating_add(duration_seconds(OUTBOX_LEASE_TTL));
        let request = OutboxLeaseRequest {
            owner: settings.lease_owner.clone(),
            class: OutboxDeliveryClass::ApprovalRequired,
            now,
            lease_until,
            limit: OUTBOX_BATCH_SIZE,
        };
        match outbox.claim(&request).await {
            Ok(messages) => {
                for message in messages {
                    deliver_outbox_message(&client, clients.as_ref(), outbox.as_ref(), &settings, &message, now).await;
                }
            }
            Err(error) => tracing::warn!(error = %error, "failed to claim durable MQTT approval notifications"),
        }
        tokio::select! {
            () = shutdown.cancelled() => return Ok(()),
            () = tokio::time::sleep(OUTBOX_POLL_INTERVAL) => {}
        }
    }
}

async fn deliver_outbox_message(
    client: &AsyncClient,
    clients: &dyn ClientStorage,
    outbox: &dyn OutboxStorage,
    settings: &OutboxSettings,
    message: &OutboxMessage,
    now: i64,
) {
    let Some(client_id) = approval_destination_client(message) else {
        // The class-filtered claim guarantees this is not a payment-event
        // row. A malformed approval destination is permanently undeliverable.
        if let Err(error) = outbox
            .mark_dead_letter(
                &message.id,
                &settings.lease_owner,
                "invalid MQTT approval notification destination",
            )
            .await
        {
            tracing::warn!(error = %error, outbox.id = %message.id, "failed to dead-letter invalid approval destination");
        }
        return;
    };
    let enabled = matches!(clients.get(&client_id).await, Ok(Some(client)) if client.enabled && client.id == client_id);
    if !enabled {
        reschedule_outbox(outbox, settings, message, now, "approval client is missing or disabled").await;
        return;
    }
    let envelope = match approval_envelope(message) {
        Ok(envelope) => envelope,
        Err(error) => {
            if let Err(storage_error) = outbox.mark_dead_letter(&message.id, &settings.lease_owner, error).await {
                tracing::warn!(error = %storage_error, outbox.id = %message.id, "failed to dead-letter invalid approval notification");
            }
            return;
        }
    };
    let payload = match serde_json::to_vec(&envelope) {
        Ok(payload) if payload.len() <= MAX_ENVELOPE_BYTES => payload,
        _ => {
            if let Err(error) = outbox
                .mark_dead_letter(
                    &message.id,
                    &settings.lease_owner,
                    "approval notification exceeds the MQTT wire limit",
                )
                .await
            {
                tracing::warn!(error = %error, outbox.id = %message.id, "failed to dead-letter oversized approval notification");
            }
            return;
        }
    };
    let topic = format!("{}/{client_id}/approvals/required", settings.topic_prefix);
    let published = tokio::time::timeout(
        settings.request_timeout,
        client.publish(topic, QoS::AtLeastOnce, false, payload),
    )
    .await;
    match published {
        Ok(Ok(())) => {
            if let Err(error) = outbox.mark_delivered(&message.id, &settings.lease_owner, now).await {
                tracing::warn!(error = %error, outbox.id = %message.id, "failed to mark MQTT approval notification delivered");
            }
        }
        Ok(Err(error)) => reschedule_outbox(outbox, settings, message, now, &error.to_string()).await,
        Err(_) => reschedule_outbox(outbox, settings, message, now, "MQTT approval publish timed out").await,
    }
}

fn approval_destination_client(message: &OutboxMessage) -> Option<String> {
    if message.event_type != "approval_required" {
        return None;
    }
    let client_id = message
        .destination
        .strip_prefix("client:")?
        .strip_suffix(".approvals")?;
    valid_identifier(client_id).then(|| client_id.to_owned())
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct ApprovalRequiredEnvelope {
    #[serde(rename = "type")]
    message_type: &'static str,
    version: u8,
    correlation_id: String,
    approval_id: String,
    intent_id: String,
    run_id: String,
    scenario_id: String,
    requested_amount: String,
    expires_at: i64,
}

fn approval_envelope(message: &OutboxMessage) -> Result<ApprovalRequiredEnvelope, &'static str> {
    let approval_id = required_payload_string(&message.payload, "approval_id")?;
    let intent_id = required_payload_string(&message.payload, "intent_id")?;
    let run_id = required_payload_string(&message.payload, "run_id")?;
    let scenario_id = required_payload_string(&message.payload, "scenario_id")?;
    if run_id != message.run_id {
        return Err("approval run does not match the outbox run");
    }
    let requested_amount = required_payload_string(&message.payload, "amount")?;
    if requested_amount.is_empty() || !requested_amount.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("approval amount is invalid");
    }
    let expires_at = message
        .payload
        .get("expires_at")
        .and_then(Value::as_i64)
        .ok_or("approval expiry is invalid")?;
    if expires_at <= message.created_at {
        return Err("approval expiry is invalid");
    }
    Ok(ApprovalRequiredEnvelope {
        message_type: "payment.approval_required",
        version: 1,
        correlation_id: message.correlation_id.clone(),
        approval_id: approval_id.to_owned(),
        intent_id: intent_id.to_owned(),
        run_id: run_id.to_owned(),
        scenario_id: scenario_id.to_owned(),
        requested_amount: requested_amount.to_owned(),
        expires_at,
    })
}

fn required_payload_string<'a>(payload: &'a Value, field: &str) -> Result<&'a str, &'static str> {
    let value = payload
        .get(field)
        .and_then(Value::as_str)
        .ok_or("approval payload field is missing")?;
    if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
        return Err("approval payload field is invalid");
    }
    Ok(value)
}

async fn reschedule_outbox(
    outbox: &dyn OutboxStorage,
    settings: &OutboxSettings,
    message: &OutboxMessage,
    now: i64,
    error: &str,
) {
    let exponent = message.attempt_count.min(8);
    let delay = 1_i64.checked_shl(exponent).unwrap_or(300).min(300);
    let bounded_error: String = error.chars().take(512).collect();
    if let Err(storage_error) = outbox
        .reschedule(
            &message.id,
            &settings.lease_owner,
            now.saturating_add(delay),
            &bounded_error,
        )
        .await
    {
        tracing::warn!(error = %storage_error, outbox.id = %message.id, "failed to reschedule MQTT approval notification");
    }
}

#[derive(Clone)]
struct MqttSettings {
    host: String,
    port: u16,
    client_id: String,
    consumer_group: String,
    topic_prefix: String,
    tls: bool,
    username: String,
    password: Zeroizing<String>,
    request_timeout: Duration,
    reconnect_delay: Duration,
}

impl MqttSettings {
    fn clone_for_task(&self) -> OutboxSettings {
        OutboxSettings {
            topic_prefix: self.topic_prefix.clone(),
            lease_owner: format!("mqtt:{}", self.client_id),
            request_timeout: self.request_timeout,
        }
    }

    fn payment_event_dispatcher_config(&self) -> PaymentEventDispatcherConfig {
        PaymentEventDispatcherConfig {
            worker_id: format!("mqtt:{}:payment-events", self.client_id),
            batch_size: OUTBOX_BATCH_SIZE,
            lease_duration: OUTBOX_LEASE_TTL,
            delivery_timeout: self.request_timeout.max(Duration::from_secs(1)),
            poll_interval: Duration::from_secs(1),
            initial_backoff: Duration::from_secs(1),
            max_backoff: Duration::from_secs(300),
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

fn validate_config(config: &MqttTransportConfig) -> crate::Result<()> {
    let loopback = config.host.eq_ignore_ascii_case("localhost")
        || IpAddr::from_str(&config.host).is_ok_and(|address| address.is_loopback());
    if !config.enabled
        || config.host.trim().is_empty()
        || config.port == 0
        || !valid_identifier(&config.client_id)
        || !valid_identifier(&config.consumer_group)
        || config.topic_prefix.is_empty()
        || config.topic_prefix.starts_with('/')
        || config.topic_prefix.ends_with('/')
        || config.topic_prefix.contains(['+', '#', '\0'])
        || config.topic_prefix.split('/').any(str::is_empty)
        || config.qos != 1
        || !valid_identifier(&config.username)
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

fn duration_seconds(duration: Duration) -> i64 {
    i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
}

fn unix_time() -> crate::Result<i64> {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| Error::InvalidRequest("system clock is before the Unix epoch".to_owned()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| Error::InvalidRequest("system clock is out of range".to_owned()))
}

#[cfg(test)]
#[path = "../../tests/controller/mqtt_test.rs"]
mod tests;
