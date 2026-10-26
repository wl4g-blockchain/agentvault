//! MQTT adapter for payment authorization and durable approval notifications.
//!
//! The broker ACL is part of the authentication boundary: a workload may
//! publish only to its own client-scoped command topic, while an approver may
//! publish only to its own client-and-principal-scoped decision topic. This
//! adapter derives both identities from the topic and never accepts a client
//! or principal claim from the message body.

use crate::{
    cache::ICache,
    cmd::{RuntimeComponent, Shutdown},
    config::MqttTransportConfig,
    payment::{PaymentApi, PaymentEventDispatcher},
    storage::{ClientStorage, OutboxStorage, ScenarioStorage},
    transport::mqtt::{
        CommandReceiver, MqttOutboxPublisher, MqttPaymentEventSink, MqttSettings, MAX_IN_FLIGHT, MAX_SECRET_BYTES,
    },
    utils::secret::SecretReader,
    Error,
};
use anyhow::{anyhow, Context as _};
use async_trait::async_trait;
use rumqttc::{AsyncClient, Event, Incoming, QoS};
use std::{fmt, sync::Arc};
use tokio::task::JoinSet;
/// Long-running MQTT transport. The contained password is resolved once from
/// an indirect secret reference and is redacted from `Debug` output.
pub struct MqttController {
    pub(super) settings: MqttSettings,
    pub(super) clients: Arc<dyn ClientStorage>,
    pub(super) scenarios: Arc<dyn ScenarioStorage>,
    pub(super) outbox: Arc<dyn OutboxStorage>,
    receiver: CommandReceiver,
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
        MqttSettings::validate_config(&config)?;
        let password = SecretReader::read_ref(&config.password, "MQTT broker password", MAX_SECRET_BYTES)?;
        if password.is_empty() {
            return Err(Error::Config("MQTT broker password must not be empty".to_owned()));
        }
        let client_id = crate::utils::runtime_identity::RuntimeIdentity::unique(&config.client_id);
        let receiver = CommandReceiver::new(
            api,
            Arc::clone(&clients),
            Arc::clone(&scenarios),
            cache,
            config.topic_prefix.clone(),
            config.request_timeout,
        );
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
            clients,
            scenarios,
            outbox,
            receiver,
        })
    }

    pub(super) async fn serve(self, mut shutdown: Shutdown) -> anyhow::Result<()> {
        let (client, mut event_loop) = AsyncClient::new(MqttSettings::mqtt_options(&self.settings), MAX_IN_FLIGHT * 2);
        let command_filters =
            MqttSettings::command_subscription_filters(&self.settings.topic_prefix, &self.settings.consumer_group);
        let decision_filter = MqttSettings::shared_subscription_filter(
            &self.settings.consumer_group,
            &format!("{}/+/approvers/+/decisions", self.settings.topic_prefix),
        );
        let mut outbox_shutdown = shutdown.clone();
        let outbox_task = tokio::spawn(MqttOutboxPublisher::run_outbox_publisher(
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
                            MqttSettings::validate_subscription_ack(&ack, command_filters.len() + 1)?;
                            tracing::info!(
                                mqtt.client_id = %self.settings.client_id,
                                mqtt.consumer_group = %self.settings.consumer_group,
                                "AgentVault MQTT command subscriptions authorized"
                            );
                        }
                        Ok(Event::Incoming(Incoming::Publish(message))) => {
                            self.receiver.handle_command_publish(&client, &mut workers, message).await;
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

    pub(super) async fn subscribe_commands(
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
}

impl fmt::Debug for MqttController {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MqttController")
            .field("settings", &self.settings)
            .field("receiver", &"<command-receiver>")
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
#[cfg(test)]
#[path = "../../tests/controller/mqtt_test.rs"]
mod tests;
