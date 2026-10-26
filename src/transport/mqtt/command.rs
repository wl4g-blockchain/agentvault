//! Tenant-bound command dispatch into the shared payment API.
use super::*;
use rumqttc::Publish;
use tracing::Instrument as _;
pub(crate) struct CommandWork {
    pub(crate) kind: CommandKind,
    pub(crate) cache_scope: String,
    pub(crate) client_id: String,
    pub(crate) approver_principal: Option<String>,
    pub(crate) request_span_id: String,
    pub(crate) traceparent: String,
    pub(crate) payload: Vec<u8>,
    pub(crate) response_topic: String,
}

pub(crate) struct CommandDependencies {
    pub(crate) client: AsyncClient,
    pub(crate) api: Arc<PaymentApi>,
    pub(crate) clients: Arc<dyn ClientStorage>,
    pub(crate) scenarios: Arc<dyn ScenarioStorage>,
    pub(crate) responses: Arc<ResponseCache>,
    pub(crate) timeout: Duration,
}

impl CommandDependencies {
    pub(crate) fn mqtt_consumer_span(work: &CommandWork) -> tracing::Span {
        let span = tracing::info_span!(
            "mqtt.server.request",
            otel.kind = "consumer",
            mqtt.client_id = %work.client_id,
            mqtt.command = ?work.kind,
            trace.producer_span_id = %work.request_span_id,
        );
        let mut headers = HeaderMap::new();
        if let Ok(value) = HeaderValue::from_str(&work.traceparent) {
            headers.insert("traceparent", value);
            set_span_parent_from_headers(&span, &headers);
        }
        span
    }

    pub(crate) async fn process_command(work: CommandWork, dependencies: CommandDependencies) {
        let response = match work.kind {
            CommandKind::Authorize => CommandDependencies::process_authorize_command(&work, &dependencies).await,
            CommandKind::OutboundResponse => CommandDependencies::process_outbound_response(&work, &dependencies).await,
            CommandKind::ResolvePrice => CommandDependencies::process_resolve_offer(&work, &dependencies).await,
            CommandKind::CreateChallenge => CommandDependencies::process_create_challenge(&work, &dependencies).await,
            CommandKind::VerifyPayment => CommandDependencies::process_verify_payment(&work, &dependencies).await,
            CommandKind::SettlePayment => CommandDependencies::process_settle_payment(&work, &dependencies).await,
            CommandKind::ApprovalDecision => CommandDependencies::process_approval_decision(&work, &dependencies).await,
        };
        dependencies
            .responses
            .complete(&work.cache_scope, &work.request_span_id, response.clone())
            .await;
        if let Err(error) = MqttResponsePublisher::publish_response(
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
                mqtt.request_span_id = %work.request_span_id,
                "failed to publish MQTT authorization response"
            );
        }
    }

    pub(crate) async fn process_authorize_command(
        work: &CommandWork,
        dependencies: &CommandDependencies,
    ) -> ResponseEnvelope {
        let command = match MqttWireCodec::parse_authorize_command(&work.payload) {
            Ok(command) => command,
            Err(error) => return MqttWireCodec::error_response(&work.request_span_id, 400, "INVALID_REQUEST", error),
        };
        match tokio::time::timeout(
            dependencies.timeout,
            CommandDependencies::authorize_for_topic_client(
                dependencies.api.as_ref(),
                dependencies.clients.as_ref(),
                &work.client_id,
                &command,
            ),
        )
        .await
        {
            Ok(Ok(value)) => {
                MqttWireCodec::success_response("payment.authorization_result", &work.request_span_id, &value)
            }
            Ok(Err(error)) => MqttWireCodec::response_for_error(&work.request_span_id, error),
            Err(_) => MqttWireCodec::error_response(
                &work.request_span_id,
                408,
                "REQUEST_TIMEOUT",
                "payment authorization timed out",
            ),
        }
    }

    pub(crate) async fn process_outbound_response(
        work: &CommandWork,
        dependencies: &CommandDependencies,
    ) -> ResponseEnvelope {
        let command = match MqttWireCodec::parse_outbound_response_command(&work.payload) {
            Ok(command) => command,
            Err(error) => return MqttWireCodec::error_response(&work.request_span_id, 400, "INVALID_REQUEST", error),
        };
        match tokio::time::timeout(
            dependencies.timeout,
            CommandDependencies::report_outbound_response_for_topic_client(
                dependencies.api.as_ref(),
                dependencies.clients.as_ref(),
                &work.client_id,
                &command,
            ),
        )
        .await
        {
            Ok(Ok(value)) => {
                MqttWireCodec::success_response("payment.outbound_response_result", &work.request_span_id, &value)
            }
            Ok(Err(error)) => MqttWireCodec::response_for_error(&work.request_span_id, error),
            Err(_) => MqttWireCodec::error_response(
                &work.request_span_id,
                408,
                "REQUEST_TIMEOUT",
                "outbound payment evidence reporting timed out",
            ),
        }
    }

    pub(crate) async fn process_resolve_offer(
        work: &CommandWork,
        dependencies: &CommandDependencies,
    ) -> ResponseEnvelope {
        let request = match MqttWireCodec::parse_payee_command::<ResolvePriceRequest>(
            &work.payload,
            "payment.resolve_offer",
            "/api/v1/offers/resolve",
        ) {
            Ok(request) => request,
            Err(error) => return MqttWireCodec::error_response(&work.request_span_id, 400, "INVALID_REQUEST", error),
        };
        let result = async {
            let principal =
                CommandDependencies::principal_for_topic_client(dependencies.clients.as_ref(), &work.client_id).await?;
            dependencies.api.resolve_offer(&principal, &request).await
        };
        CommandDependencies::timed_payment_response(
            dependencies.timeout,
            result,
            "payment.offer_resolved",
            &work.request_span_id,
            "offer resolution timed out",
        )
        .await
    }

    pub(crate) async fn process_create_challenge(
        work: &CommandWork,
        dependencies: &CommandDependencies,
    ) -> ResponseEnvelope {
        let request = match MqttWireCodec::parse_payee_command::<CreateChallengeRequest>(
            &work.payload,
            "payment.create_challenge",
            "/api/v1/payee/challenges",
        ) {
            Ok(request) => request,
            Err(error) => return MqttWireCodec::error_response(&work.request_span_id, 400, "INVALID_REQUEST", error),
        };
        let result = async {
            let principal =
                CommandDependencies::principal_for_topic_client(dependencies.clients.as_ref(), &work.client_id).await?;
            dependencies
                .api
                .create_challenge(&principal, &request, MqttSettings::unix_time()?)
                .await
        };
        CommandDependencies::timed_payment_response(
            dependencies.timeout,
            result,
            "payment.challenge_created",
            &work.request_span_id,
            "challenge creation timed out",
        )
        .await
    }

    pub(crate) async fn process_verify_payment(
        work: &CommandWork,
        dependencies: &CommandDependencies,
    ) -> ResponseEnvelope {
        let request = match MqttWireCodec::parse_payee_command::<VerifyPaymentRequest>(
            &work.payload,
            "payment.verify",
            "/api/v1/payee/verify",
        ) {
            Ok(request) => request,
            Err(error) => return MqttWireCodec::error_response(&work.request_span_id, 400, "INVALID_REQUEST", error),
        };
        let result = async {
            let principal =
                CommandDependencies::principal_for_topic_client(dependencies.clients.as_ref(), &work.client_id).await?;
            dependencies
                .api
                .verify_payment(&principal, &request, MqttSettings::unix_time()?)
                .await
        };
        CommandDependencies::timed_payment_response(
            dependencies.timeout,
            result,
            "payment.verification_result",
            &work.request_span_id,
            "payment verification timed out",
        )
        .await
    }

    pub(crate) async fn process_settle_payment(
        work: &CommandWork,
        dependencies: &CommandDependencies,
    ) -> ResponseEnvelope {
        let request = match MqttWireCodec::parse_payee_command::<SettlePaymentRequest>(
            &work.payload,
            "payment.settle",
            "/api/v1/payee/settle",
        ) {
            Ok(request) => request,
            Err(error) => return MqttWireCodec::error_response(&work.request_span_id, 400, "INVALID_REQUEST", error),
        };
        let result = async {
            let principal =
                CommandDependencies::principal_for_topic_client(dependencies.clients.as_ref(), &work.client_id).await?;
            dependencies
                .api
                .settle_payment(&principal, &request, MqttSettings::unix_time()?)
                .await
        };
        CommandDependencies::timed_payment_response(
            dependencies.timeout,
            result,
            "payment.settlement_result",
            &work.request_span_id,
            "payment settlement timed out",
        )
        .await
    }

    pub(crate) async fn timed_payment_response<T, F>(
        timeout: Duration,
        operation: F,
        response_type: &'static str,
        request_span_id: &str,
        timeout_message: &'static str,
    ) -> ResponseEnvelope
    where
        T: Serialize,
        F: std::future::Future<Output = crate::Result<T>>,
    {
        match tokio::time::timeout(timeout, operation).await {
            Ok(Ok(value)) => MqttWireCodec::success_response(response_type, request_span_id, &value),
            Ok(Err(error)) => MqttWireCodec::response_for_error(request_span_id, error),
            Err(_) => MqttWireCodec::error_response(request_span_id, 408, "REQUEST_TIMEOUT", timeout_message),
        }
    }

    pub(crate) async fn principal_for_topic_client(
        clients: &dyn ClientStorage,
        client_id: &str,
    ) -> crate::Result<String> {
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

    pub(crate) async fn process_approval_decision(
        work: &CommandWork,
        dependencies: &CommandDependencies,
    ) -> ResponseEnvelope {
        let command = match MqttWireCodec::parse_decision_command(&work.payload) {
            Ok(command) => command,
            Err(error) => {
                return MqttWireCodec::error_response(&work.request_span_id, error.status, error.code, error.message)
            }
        };
        match tokio::time::timeout(
            dependencies.timeout,
            CommandDependencies::decide_for_topic_client(
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
            Ok(Ok(value)) => {
                MqttWireCodec::success_response("payment.approval_decision_result", &work.request_span_id, &value)
            }
            Ok(Err(error)) => MqttWireCodec::response_for_error(&work.request_span_id, error),
            Err(_) => MqttWireCodec::error_response(
                &work.request_span_id,
                408,
                "REQUEST_TIMEOUT",
                "approval decision timed out",
            ),
        }
    }

    pub(crate) async fn authorize_for_topic_client(
        api: &PaymentApi,
        clients: &dyn ClientStorage,
        client_id: &str,
        request: &AuthorizePaymentRequest,
    ) -> crate::Result<crate::model::payment_api::AuthorizePaymentResponse> {
        let client = clients
            .get(client_id)
            .await?
            .filter(|client| client.enabled)
            .ok_or(Error::Forbidden)?;
        if client.id != client_id || client.auth_principal.trim().is_empty() {
            return Err(Error::Forbidden);
        }
        api.authorize(&client.auth_principal, request, MqttSettings::unix_time()?)
            .await
    }

    pub(crate) async fn report_outbound_response_for_topic_client(
        api: &PaymentApi,
        clients: &dyn ClientStorage,
        client_id: &str,
        command: &OutboundResponseCommand,
    ) -> crate::Result<crate::model::payment_api::ReportOutboundResponseResponse> {
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
            MqttSettings::unix_time()?,
        )
        .await
    }

    pub(crate) async fn decide_for_topic_client(
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
        api.decide_approval(approver_principal, &command.request, MqttSettings::unix_time()?)
            .await
    }
}

#[cfg(test)]
#[path = "../../../tests/transport/mqtt/command_test.rs"]
mod tests;

/// Bounded wire-command reception. `QoS` replay state remains ephemeral; the
/// application component continues to enforce durable `run_id` idempotency.
pub(crate) struct CommandReceiver {
    api: Arc<PaymentApi>,
    clients: Arc<dyn ClientStorage>,
    scenarios: Arc<dyn ScenarioStorage>,
    limiter: ConcurrencyLimiter,
    responses: Arc<ResponseCache>,
    topic_prefix: String,
    timeout: Duration,
}
impl CommandReceiver {
    pub(crate) fn new(
        api: Arc<PaymentApi>,
        clients: Arc<dyn ClientStorage>,
        scenarios: Arc<dyn ScenarioStorage>,
        cache: Arc<dyn ICache>,
        topic_prefix: String,
        timeout: Duration,
    ) -> Self {
        Self {
            api,
            clients,
            scenarios,
            limiter: ConcurrencyLimiter::new(),
            responses: Arc::new(ResponseCache::new(cache)),
            topic_prefix,
            timeout,
        }
    }
    pub(crate) async fn handle_command_publish(
        &self,
        client: &AsyncClient,
        workers: &mut JoinSet<()>,
        message: Publish,
    ) {
        if message.qos != QoS::AtLeastOnce || message.retain {
            tracing::warn!(mqtt.topic = %message.topic, "rejected MQTT command without strict QoS 1 semantics");
            return;
        }
        if message.payload.len() > MAX_ENVELOPE_BYTES {
            tracing::warn!(mqtt.topic = %message.topic, "rejected oversized MQTT command");
            return;
        }
        let Some(command) = MqttWireCodec::command_topic(&self.topic_prefix, &message.topic) else {
            tracing::warn!(mqtt.topic = %message.topic, "rejected MQTT command on an invalid topic");
            return;
        };
        let client_id = command.client_id().to_owned();
        let approver_principal = command.approver_principal().map(str::to_owned);
        let kind = command.kind();
        let cache_scope = command.cache_scope();
        let Some((request_span_id, traceparent, trace_id)) = MqttWireCodec::probe_trace_context(&message.payload)
        else {
            tracing::warn!(mqtt.topic = %message.topic, "rejected MQTT command without a bound W3C producer span");
            return;
        };
        let response_topic = command.response_topic(&self.topic_prefix, &request_span_id);
        tracing::info!(
            event.name = "agentvault.transport.mqtt.request",
            mqtt.client_id = %client_id,
            mqtt.command = ?kind,
            trace.id = %trace_id,
            trace.producer_span_id = %request_span_id,
            "received AgentVault MQTT payment request"
        );
        match self
            .responses
            .begin(&cache_scope, &request_span_id, ResponseCache::digest(&message.payload))
            .await
        {
            CacheDisposition::Cached(response) => {
                MqttResponsePublisher::spawn_publish(workers, client.clone(), response_topic, response, self.timeout);
            }
            CacheDisposition::InFlight => {
                tracing::debug!(mqtt.client_id = %client_id, mqtt.request_span_id = %request_span_id, "coalesced duplicate MQTT command");
            }
            CacheDisposition::Conflict => {
                let response = MqttWireCodec::error_response(
                    &request_span_id,
                    409,
                    "PAYMENT_CONFLICT",
                    "request span id was reused with different content",
                );
                MqttResponsePublisher::spawn_publish(workers, client.clone(), response_topic, response, self.timeout);
            }
            CacheDisposition::New => {
                let work = CommandWork {
                    kind,
                    cache_scope,
                    client_id,
                    approver_principal,
                    request_span_id,
                    traceparent,
                    payload: message.payload.to_vec(),
                    response_topic,
                };
                self.spawn_new_command(client, workers, work).await;
            }
        }
    }
    pub(crate) async fn spawn_new_command(&self, client: &AsyncClient, workers: &mut JoinSet<()>, work: CommandWork) {
        let Some(permits) = self.limiter.try_acquire(&work.client_id).await else {
            let response = MqttWireCodec::error_response(
                &work.request_span_id,
                429,
                "RATE_LIMITED",
                "too many concurrent MQTT payment requests",
            );
            self.responses
                .complete(&work.cache_scope, &work.request_span_id, response.clone())
                .await;
            MqttResponsePublisher::spawn_publish(workers, client.clone(), work.response_topic, response, self.timeout);
            return;
        };
        let dependencies = CommandDependencies {
            client: client.clone(),
            api: Arc::clone(&self.api),
            clients: Arc::clone(&self.clients),
            scenarios: Arc::clone(&self.scenarios),
            responses: Arc::clone(&self.responses),
            timeout: self.timeout,
        };
        let span = CommandDependencies::mqtt_consumer_span(&work);
        workers.spawn(
            async move {
                let _permits = permits;
                CommandDependencies::process_command(work, dependencies).await;
            }
            .instrument(span),
        );
    }
}
