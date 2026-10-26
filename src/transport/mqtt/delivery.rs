//! `QoS` 1 responses and durable event/approval notification delivery.
use super::*;
#[derive(Clone)]
pub(crate) struct OutboxSettings {
    pub(crate) topic_prefix: String,
    pub(crate) lease_owner: String,
    pub(crate) request_timeout: Duration,
}

pub(crate) struct MqttPaymentEventSink {
    pub(crate) client: AsyncClient,
    pub(crate) clients: Arc<dyn IClientRepository>,
    pub(crate) scenarios: Arc<dyn IScenarioRepository>,
    pub(crate) topic_prefix: String,
}

pub(crate) enum PaymentEventDestination<'a> {
    Client(&'a str),
    Scenario(&'a str),
}

#[async_trait]
impl PaymentEventSink for MqttPaymentEventSink {
    async fn deliver(&self, message: &OutboxMessage) -> PaymentEventDelivery {
        let destination = match MqttPaymentEventSink::payment_event_destination(&message.destination) {
            Ok(destination) => destination,
            Err(reason) => return MqttPaymentEventSink::permanent_delivery_failure(reason),
        };
        let client_id = match self.resolve_client(destination).await {
            Ok(client_id) => client_id,
            Err(disposition) => return disposition,
        };
        if !MqttWireCodec::valid_identifier(&message.event_type) {
            return MqttPaymentEventSink::permanent_delivery_failure(
                "payment event type is not a safe MQTT topic segment",
            );
        }
        let envelope = match MqttPaymentEventSink::payment_event_envelope(message) {
            Ok(envelope) => envelope,
            Err(reason) => return MqttPaymentEventSink::permanent_delivery_failure(reason),
        };
        let payload = match serde_json::to_vec(&envelope) {
            Ok(payload) if payload.len() <= MAX_ENVELOPE_BYTES => payload,
            Ok(_) => {
                return MqttPaymentEventSink::permanent_delivery_failure("payment event exceeds the MQTT wire limit")
            }
            Err(_) => return MqttPaymentEventSink::permanent_delivery_failure("payment event cannot be encoded"),
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
    pub(crate) async fn resolve_client(
        &self,
        destination: PaymentEventDestination<'_>,
    ) -> Result<String, PaymentEventDelivery> {
        let client_id = match destination {
            PaymentEventDestination::Client(client_id) => client_id.to_owned(),
            PaymentEventDestination::Scenario(scenario_id) => match self.scenarios.get(scenario_id).await {
                Ok(Some(scenario))
                    if scenario.id == scenario_id && MqttWireCodec::valid_identifier(&scenario.client_id) =>
                {
                    scenario.client_id
                }
                Ok(Some(_)) => {
                    return Err(MqttPaymentEventSink::permanent_delivery_failure(
                        "payment event scenario ownership is invalid",
                    ));
                }
                Ok(None) => {
                    return Err(MqttPaymentEventSink::permanent_delivery_failure(
                        "payment event scenario does not exist",
                    ))
                }
                Err(error) => {
                    return Err(MqttPaymentEventSink::retryable_delivery_failure(format!(
                        "resolve payment event scenario: {error}"
                    )));
                }
            },
        };
        match self.clients.get(&client_id).await {
            Ok(Some(client)) if client.id == client_id && client.enabled => Ok(client_id),
            Ok(Some(client)) if client.id == client_id => Err(MqttPaymentEventSink::retryable_delivery_failure(
                "payment event client is disabled".to_owned(),
            )),
            Ok(Some(_)) => Err(MqttPaymentEventSink::permanent_delivery_failure(
                "payment event client ownership is invalid",
            )),
            Ok(None) => Err(MqttPaymentEventSink::permanent_delivery_failure(
                "payment event client does not exist",
            )),
            Err(error) => Err(MqttPaymentEventSink::retryable_delivery_failure(format!(
                "resolve payment event client: {error}"
            ))),
        }
    }
}

pub(crate) struct MqttResponsePublisher;

impl MqttResponsePublisher {
    pub(crate) fn spawn_publish(
        workers: &mut JoinSet<()>,
        client: AsyncClient,
        topic: String,
        response: ResponseEnvelope,
        timeout: Duration,
    ) {
        workers.spawn(async move {
            if let Err(error) = MqttResponsePublisher::publish_response(&client, &topic, &response, timeout).await {
                tracing::warn!(error = %error, mqtt.topic = %topic, "failed to republish MQTT authorization response");
            }
        });
    }

    pub(crate) async fn publish_response(
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
}

impl MqttPaymentEventSink {
    pub(crate) fn payment_event_destination(destination: &str) -> Result<PaymentEventDestination<'_>, &'static str> {
        if let Some(identifier) = destination
            .strip_prefix("client:")
            .and_then(|value| value.strip_suffix(".payments"))
        {
            return MqttWireCodec::valid_identifier(identifier)
                .then_some(PaymentEventDestination::Client(identifier))
                .ok_or("payment event destination identifier is invalid");
        }
        if let Some(identifier) = destination
            .strip_prefix("scenario:")
            .and_then(|value| value.strip_suffix(".payments"))
        {
            return MqttWireCodec::valid_identifier(identifier)
                .then_some(PaymentEventDestination::Scenario(identifier))
                .ok_or("payment event destination identifier is invalid");
        }
        Err("payment event destination is invalid")
    }

    pub(crate) fn payment_event_envelope(message: &OutboxMessage) -> Result<PaymentEventEnvelope<'_>, &'static str> {
        for value in [
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
            run_id: &message.run_id,
            aggregate_type: &message.aggregate_type,
            aggregate_id: &message.aggregate_id,
            event_type: &message.event_type,
            payload: &message.payload,
        })
    }

    pub(crate) fn permanent_delivery_failure(reason: &'static str) -> PaymentEventDelivery {
        PaymentEventDelivery::PermanentFailure {
            reason: reason.to_owned(),
        }
    }

    pub(crate) fn retryable_delivery_failure(reason: String) -> PaymentEventDelivery {
        PaymentEventDelivery::Retryable { reason }
    }
}

pub(crate) struct MqttOutboxPublisher;

impl MqttOutboxPublisher {
    pub(crate) async fn run_outbox_publisher(
        client: AsyncClient,
        clients: Arc<dyn IClientRepository>,
        outbox: Arc<dyn IOutboxRepository>,
        settings: OutboxSettings,
        mut shutdown: Shutdown,
    ) -> anyhow::Result<()> {
        loop {
            let now = MqttSettings::unix_time().map_err(|error| anyhow!(error))?;
            let lease_until = now.saturating_add(MqttSettings::duration_seconds(OUTBOX_LEASE_TTL));
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
                        MqttOutboxPublisher::deliver_outbox_message(
                            &client,
                            clients.as_ref(),
                            outbox.as_ref(),
                            &settings,
                            &message,
                            now,
                        )
                        .await;
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

    pub(crate) async fn deliver_outbox_message(
        client: &AsyncClient,
        clients: &dyn IClientRepository,
        outbox: &dyn IOutboxRepository,
        settings: &OutboxSettings,
        message: &OutboxMessage,
        now: i64,
    ) {
        let Some(client_id) = MqttOutboxPublisher::approval_destination_client(message) else {
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
        let enabled =
            matches!(clients.get(&client_id).await, Ok(Some(client)) if client.enabled && client.id == client_id);
        if !enabled {
            MqttOutboxPublisher::reschedule_outbox(
                outbox,
                settings,
                message,
                now,
                "approval client is missing or disabled",
            )
            .await;
            return;
        }
        let envelope = match MqttOutboxPublisher::approval_envelope(message) {
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
            Ok(Err(error)) => {
                MqttOutboxPublisher::reschedule_outbox(outbox, settings, message, now, &error.to_string()).await;
            }
            Err(_) => {
                MqttOutboxPublisher::reschedule_outbox(
                    outbox,
                    settings,
                    message,
                    now,
                    "MQTT approval publish timed out",
                )
                .await;
            }
        }
    }

    pub(crate) fn approval_destination_client(message: &OutboxMessage) -> Option<String> {
        if message.event_type != "approval_required" {
            return None;
        }
        let client_id = message
            .destination
            .strip_prefix("client:")?
            .strip_suffix(".approvals")?;
        MqttWireCodec::valid_identifier(client_id).then(|| client_id.to_owned())
    }

    pub(crate) fn approval_envelope(message: &OutboxMessage) -> Result<ApprovalRequiredEnvelope, &'static str> {
        let approval_id = MqttOutboxPublisher::required_payload_string(&message.payload, "approval_id")?;
        let intent_id = MqttOutboxPublisher::required_payload_string(&message.payload, "intent_id")?;
        let run_id = MqttOutboxPublisher::required_payload_string(&message.payload, "run_id")?;
        let scenario_id = MqttOutboxPublisher::required_payload_string(&message.payload, "scenario_id")?;
        if run_id != message.run_id {
            return Err("approval run does not match the outbox run");
        }
        let requested_amount = MqttOutboxPublisher::required_payload_string(&message.payload, "amount")?;
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
            approval_id: approval_id.to_owned(),
            intent_id: intent_id.to_owned(),
            run_id: run_id.to_owned(),
            scenario_id: scenario_id.to_owned(),
            requested_amount: requested_amount.to_owned(),
            expires_at,
        })
    }

    pub(crate) fn required_payload_string<'a>(payload: &'a Value, field: &str) -> Result<&'a str, &'static str> {
        let value = payload
            .get(field)
            .and_then(Value::as_str)
            .ok_or("approval payload field is missing")?;
        if value.is_empty() || value.len() > 512 || value.chars().any(char::is_control) {
            return Err("approval payload field is invalid");
        }
        Ok(value)
    }

    pub(crate) async fn reschedule_outbox(
        outbox: &dyn IOutboxRepository,
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
}

#[cfg(test)]
#[path = "../../../tests/transport/mqtt/delivery_test.rs"]
mod tests;
