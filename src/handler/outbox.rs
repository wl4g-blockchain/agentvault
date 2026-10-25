use std::{future::Future, sync::Arc, time::Duration};

use async_trait::async_trait;

use crate::{
    model::{PaymentEventDelivery, PaymentEventDispatchRun},
    storage::{OutboxDeliveryClass, OutboxLeaseRequest, OutboxMessage, OutboxStorage},
    Error, Result,
};

const MAX_BATCH_SIZE: u32 = 1_000;
const MAX_ERROR_CHARS: usize = 512;

/// Runtime limits for the durable payment-event outbox worker.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PaymentEventDispatcherConfig {
    pub worker_id: String,
    pub batch_size: u32,
    pub lease_duration: Duration,
    pub delivery_timeout: Duration,
    pub poll_interval: Duration,
    pub initial_backoff: Duration,
    pub max_backoff: Duration,
    pub max_attempts: u32,
}

impl PaymentEventDispatcherConfig {
    /// Validates bounded worker and persisted timestamp settings.
    ///
    /// # Errors
    ///
    /// Returns a configuration error for an invalid worker id, limit, or
    /// duration.
    pub fn validate(&self) -> Result<()> {
        if self.worker_id.trim().is_empty() {
            return Err(Error::Config(
                "payment event dispatcher worker_id must not be empty".to_owned(),
            ));
        }
        if self.batch_size == 0 || self.batch_size > MAX_BATCH_SIZE {
            return Err(Error::Config(format!(
                "payment event dispatcher batch_size must be between 1 and {MAX_BATCH_SIZE}"
            )));
        }
        if self.max_attempts == 0 {
            return Err(Error::Config(
                "payment event dispatcher max_attempts must be positive".to_owned(),
            ));
        }
        duration_seconds(self.lease_duration, "lease_duration")?;
        duration_seconds(self.delivery_timeout, "delivery_timeout")?;
        duration_seconds(self.poll_interval, "poll_interval")?;
        let initial = duration_seconds(self.initial_backoff, "initial_backoff")?;
        let maximum = duration_seconds(self.max_backoff, "max_backoff")?;
        if initial > maximum {
            return Err(Error::Config(
                "payment event dispatcher initial_backoff must not exceed max_backoff".to_owned(),
            ));
        }
        Ok(())
    }
}

/// Application/provider transport boundary for one durable payment event.
/// Implementations must deduplicate by
/// `(message.run_id, message.event_type)` because a crash after remote
/// delivery and before local acknowledgement causes replay.
#[async_trait]
pub trait PaymentEventSink: Send + Sync {
    async fn deliver(&self, message: &OutboxMessage) -> PaymentEventDelivery;
}

/// Class-filtered, at-least-once dispatcher for non-approval payment events.
/// A runtime may start it only after installing a concrete idempotent sink.
pub struct PaymentEventDispatcher {
    storage: Arc<dyn OutboxStorage>,
    sink: Arc<dyn PaymentEventSink>,
    config: PaymentEventDispatcherConfig,
    lease_seconds: i64,
    delivery_timeout: Duration,
    initial_backoff_seconds: i64,
    max_backoff_seconds: i64,
}

impl PaymentEventDispatcher {
    /// Creates a dispatcher without claiming any rows.
    ///
    /// # Errors
    ///
    /// Returns a configuration error when worker limits are invalid.
    pub fn new(
        storage: Arc<dyn OutboxStorage>,
        sink: Arc<dyn PaymentEventSink>,
        config: PaymentEventDispatcherConfig,
    ) -> Result<Self> {
        config.validate()?;
        Ok(Self {
            storage,
            sink,
            lease_seconds: duration_seconds(config.lease_duration, "lease_duration")?,
            delivery_timeout: config.delivery_timeout,
            initial_backoff_seconds: duration_seconds(config.initial_backoff, "initial_backoff")?,
            max_backoff_seconds: duration_seconds(config.max_backoff, "max_backoff")?,
            config,
        })
    }

    /// Claims and dispatches one bounded `PaymentEvents` batch.
    ///
    /// # Errors
    ///
    /// Fails closed for an invalid timestamp, storage/lease failure, or a
    /// storage adapter returning a foreign delivery class.
    pub async fn run_once(&self, now: i64) -> Result<PaymentEventDispatchRun> {
        if now < 0 {
            return Err(Error::InvalidRequest(
                "payment event dispatcher timestamp must be non-negative".to_owned(),
            ));
        }
        let messages = self
            .storage
            .claim(&OutboxLeaseRequest {
                owner: self.config.worker_id.clone(),
                class: OutboxDeliveryClass::PaymentEvents,
                now,
                lease_until: checked_add(now, self.lease_seconds, "payment event outbox lease")?,
                limit: self.config.batch_size,
            })
            .await?;
        let mut run = PaymentEventDispatchRun {
            claimed: u32::try_from(messages.len()).unwrap_or(u32::MAX),
            ..PaymentEventDispatchRun::default()
        };
        for message in messages {
            if !OutboxDeliveryClass::PaymentEvents.matches(&message) {
                return Err(Error::Storage(
                    "payment event outbox claim returned a foreign delivery class".to_owned(),
                ));
            }
            let disposition = match tokio::time::timeout(self.delivery_timeout, self.sink.deliver(&message)).await {
                Ok(disposition) => disposition,
                Err(_) => PaymentEventDelivery::Retryable {
                    reason: "payment event delivery timed out".to_owned(),
                },
            };
            self.persist_disposition(&message, disposition, now, &mut run).await?;
        }
        Ok(run)
    }

    /// Polls durable work until `shutdown` resolves.
    ///
    /// # Errors
    ///
    /// Returns the first authoritative clock, claim, or lease-transition
    /// failure. Sink retry/permanent outcomes are persisted and do not stop
    /// the loop.
    pub async fn run_until<F>(&self, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()> + Send,
    {
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                biased;
                () = &mut shutdown => return Ok(()),
                result = self.run_once(unix_time()?) => {
                    result?;
                }
            }
            tokio::select! {
                biased;
                () = &mut shutdown => return Ok(()),
                () = tokio::time::sleep(self.config.poll_interval) => {}
            }
        }
    }

    async fn persist_disposition(
        &self,
        message: &OutboxMessage,
        disposition: PaymentEventDelivery,
        now: i64,
        run: &mut PaymentEventDispatchRun,
    ) -> Result<()> {
        match disposition {
            PaymentEventDelivery::Delivered => {
                self.storage
                    .mark_delivered(&message.id, &self.config.worker_id, now)
                    .await?;
                run.delivered = run.delivered.saturating_add(1);
            }
            PaymentEventDelivery::Retryable { reason } if message.attempt_count < self.config.max_attempts => {
                let available_at = checked_add(now, self.retry_delay(message.attempt_count), "outbox retry")?;
                self.storage
                    .reschedule(
                        &message.id,
                        &self.config.worker_id,
                        available_at,
                        &bounded_reason(&reason),
                    )
                    .await?;
                run.rescheduled = run.rescheduled.saturating_add(1);
            }
            PaymentEventDelivery::Retryable { reason } => {
                let reason = format!("delivery attempts exhausted: {}", bounded_reason(&reason));
                self.dead_letter(message, &reason).await?;
                run.dead_lettered = run.dead_lettered.saturating_add(1);
            }
            PaymentEventDelivery::PermanentFailure { reason } => {
                self.dead_letter(message, &reason).await?;
                run.dead_lettered = run.dead_lettered.saturating_add(1);
            }
        }
        Ok(())
    }

    async fn dead_letter(&self, message: &OutboxMessage, reason: &str) -> Result<()> {
        self.storage
            .mark_dead_letter(&message.id, &self.config.worker_id, &bounded_reason(reason))
            .await
    }

    fn retry_delay(&self, attempt_count: u32) -> i64 {
        let exponent = attempt_count.saturating_sub(1).min(62);
        let multiplier = 1_i64.checked_shl(exponent).unwrap_or(i64::MAX);
        self.initial_backoff_seconds
            .saturating_mul(multiplier)
            .min(self.max_backoff_seconds)
    }
}

fn duration_seconds(duration: Duration, field: &str) -> Result<i64> {
    let seconds = i64::try_from(duration.as_secs())
        .map_err(|_| Error::Config(format!("payment event dispatcher {field} is too large")))?;
    if seconds == 0 {
        return Err(Error::Config(format!(
            "payment event dispatcher {field} must be at least one second"
        )));
    }
    Ok(seconds)
}

fn checked_add(timestamp: i64, delta: i64, field: &str) -> Result<i64> {
    timestamp
        .checked_add(delta)
        .ok_or_else(|| Error::Storage(format!("{field} timestamp overflow")))
}

fn bounded_reason(reason: &str) -> String {
    let value: String = reason.chars().take(MAX_ERROR_CHARS).collect();
    if value.trim().is_empty() {
        "payment event delivery failed".to_owned()
    } else {
        value
    }
}

fn unix_time() -> Result<i64> {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| Error::InvalidRequest("system clock is before the Unix epoch".to_owned()))?
        .as_secs();
    i64::try_from(seconds).map_err(|_| Error::InvalidRequest("system clock is out of range".to_owned()))
}

#[cfg(test)]
#[path = "../../tests/handler/outbox_test.rs"]
mod tests;
