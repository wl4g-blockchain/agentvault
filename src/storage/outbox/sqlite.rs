//! Outbox persistence for `sqlite`.

use crate::{
    storage::{sqlite::SqliteRepository, IOutboxRepository, OutboxLeaseRequest, OutboxMessage},
    Error, Result,
};
use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

#[derive(Clone)]
pub struct OutboxSqliteRepository {
    base: SqliteRepository,
}

#[async_trait]
impl IOutboxRepository for OutboxSqliteRepository {
    async fn claim(&self, request: &OutboxLeaseRequest) -> Result<Vec<OutboxMessage>> {
        if request.owner.trim().is_empty() || request.lease_until <= request.now || request.limit == 0 {
            return Err(Error::InvalidRequest("invalid outbox lease request".to_owned()));
        }
        let request = request.clone();
        self.base
            .immediate(|connection| {
                Box::pin(async move {
                    let ids: Vec<String> = sqlx::query_scalar(
                        "SELECT id FROM pay_outbox WHERE status = 'pending' AND available_at <= ?1 \
                     AND (lease_expires_at IS NULL OR lease_expires_at <= ?1) \
                     AND ((?3 = 'approval_required' AND event_type = 'approval_required' \
                           AND destination GLOB 'client:?*.approvals') \
                          OR (?3 = 'payment_events' AND (destination GLOB 'client:?*.payments' \
                              OR destination GLOB 'scenario:?*.payments'))) \
                     ORDER BY available_at, created_at, id LIMIT ?2",
                    )
                    .bind(request.now)
                    .bind(i64::from(request.limit))
                    .bind(request.class.as_str())
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(|error| SqliteRepository::storage_error("select due outbox messages", error))?;
                    let mut claimed = Vec::with_capacity(ids.len());
                    for id in ids {
                        let result = sqlx::query(
                            "UPDATE pay_outbox SET lease_owner = ?1, lease_expires_at = ?2, \
                         attempt_count = attempt_count + 1 WHERE id = ?3 AND status = 'pending' \
                         AND available_at <= ?4 AND (lease_expires_at IS NULL OR lease_expires_at <= ?4) \
                         AND attempt_count < 4294967295",
                        )
                        .bind(&request.owner)
                        .bind(request.lease_until)
                        .bind(&id)
                        .bind(request.now)
                        .execute(&mut *connection)
                        .await
                        .map_err(|error| SqliteRepository::storage_error("claim outbox message", error))?;
                        if result.rows_affected() != 1 {
                            return Err(crate::Error::conflict("outbox claim lost its conditional lease update"));
                        }
                        let row = sqlx::query("SELECT * FROM pay_outbox WHERE id = ?1")
                            .bind(&id)
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(|error| SqliteRepository::storage_error("reload claimed outbox message", error))?;
                        claimed.push(Self::decode_row(&row)?);
                    }
                    Ok(claimed)
                })
            })
            .await
    }

    async fn mark_delivered(&self, message_id: &str, owner: &str, delivered_at: i64) -> Result<()> {
        Self::transition_lease(
            self,
            "UPDATE pay_outbox SET status = 'delivered', delivered_at = ?1, lease_owner = NULL, \
             lease_expires_at = NULL, last_error = NULL WHERE id = ?2 AND status = 'pending' AND lease_owner = ?3",
            message_id,
            owner,
            delivered_at,
            None,
            "mark outbox message delivered",
        )
        .await
    }

    async fn reschedule(&self, message_id: &str, owner: &str, available_at: i64, error: &str) -> Result<()> {
        Self::transition_lease(
            self,
            "UPDATE pay_outbox SET available_at = ?1, lease_owner = NULL, lease_expires_at = NULL, \
             last_error = ?4 WHERE id = ?2 AND status = 'pending' AND lease_owner = ?3",
            message_id,
            owner,
            available_at,
            Some(error),
            "reschedule outbox message",
        )
        .await
    }

    async fn mark_dead_letter(&self, message_id: &str, owner: &str, error: &str) -> Result<()> {
        Self::transition_lease(
            self,
            "UPDATE pay_outbox SET status = 'dead_letter', lease_owner = NULL, lease_expires_at = NULL, \
             last_error = ?4 WHERE id = ?2 AND status = 'pending' AND lease_owner = ?3",
            message_id,
            owner,
            0,
            Some(error),
            "mark outbox message dead-letter",
        )
        .await
    }
}

impl OutboxSqliteRepository {
    pub(crate) fn new(base: SqliteRepository) -> Self {
        Self { base }
    }

    pub(crate) async fn insert_outbox(connection: &mut sqlx::SqliteConnection, message: &OutboxMessage) -> Result<()> {
        OutboxStorageRules::validate_message(message)?;
        let existing = sqlx::query(
            "SELECT * FROM pay_outbox WHERE id = ?1 OR (destination = ?2 AND event_type = ?3 AND run_id = ?4)",
        )
        .bind(&message.id)
        .bind(&message.destination)
        .bind(&message.event_type)
        .bind(&message.run_id)
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| SqliteRepository::storage_error("find outbox replay", error))?;
        if !existing.is_empty() {
            if existing.len() != 1
                || !OutboxStorageRules::same_logical_message(&Self::decode_row(&existing[0])?, message)
            {
                return Err(crate::Error::conflict("outbox run identity has different content"));
            }
            return Ok(());
        }
        let payload = SqliteRepository::encode_json(&message.payload, "pay_outbox.payload")?;
        sqlx::query(
            "INSERT INTO pay_outbox (id, aggregate_type, aggregate_id, event_type, destination, \
     run_id, payload, status, attempt_count, available_at, lease_owner, lease_expires_at, \
     last_error, created_at, delivered_at) \
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)",
        )
        .bind(&message.id)
        .bind(&message.aggregate_type)
        .bind(&message.aggregate_id)
        .bind(&message.event_type)
        .bind(&message.destination)
        .bind(&message.run_id)
        .bind(payload)
        .bind(message.status.as_str())
        .bind(i64::from(message.attempt_count))
        .bind(message.available_at)
        .bind(&message.lease_owner)
        .bind(message.lease_expires_at)
        .bind(&message.last_error)
        .bind(message.created_at)
        .bind(message.delivered_at)
        .execute(connection)
        .await
        .map_err(|error| crate::Error::conflict(format!("outbox insert rejected: {error}")))?;
        Ok(())
    }

    async fn transition_lease(
        storage: &OutboxSqliteRepository,
        sql: &'static str,
        message_id: &str,
        owner: &str,
        timestamp: i64,
        error: Option<&str>,
        operation: &'static str,
    ) -> Result<()> {
        if message_id.trim().is_empty() || owner.trim().is_empty() {
            return Err(Error::InvalidRequest("outbox lease identity is incomplete".to_owned()));
        }
        let message_id = message_id.to_owned();
        let owner = owner.to_owned();
        let error = error.map(str::to_owned);
        storage
            .base
            .immediate(|connection| {
                Box::pin(async move {
                    let result = sqlx::query(sql)
                        .bind(timestamp)
                        .bind(&message_id)
                        .bind(&owner)
                        .bind(error)
                        .execute(&mut *connection)
                        .await
                        .map_err(|source| SqliteRepository::storage_error(operation, source))?;
                    if result.rows_affected() != 1 {
                        return Err(crate::Error::conflict("outbox message is not held by the lease owner"));
                    }
                    Ok(())
                })
            })
            .await
    }

    pub(crate) fn decode_row(row: &SqliteRow) -> Result<OutboxMessage> {
        Ok(OutboxMessage {
            id: SqliteRepository::field(row, "id")?,
            aggregate_type: SqliteRepository::field(row, "aggregate_type")?,
            aggregate_id: SqliteRepository::field(row, "aggregate_id")?,
            event_type: SqliteRepository::field(row, "event_type")?,
            destination: SqliteRepository::field(row, "destination")?,
            run_id: SqliteRepository::field(row, "run_id")?,
            payload: SqliteRepository::decode_json(
                &SqliteRepository::field::<String>(row, "payload")?,
                "pay_outbox.payload",
            )?,
            status: PaymentStorageCodec::outbox_status(&SqliteRepository::field::<String>(row, "status")?)?,
            attempt_count: SqliteRepository::decode_u32(
                SqliteRepository::field(row, "attempt_count")?,
                "pay_outbox.attempt_count",
            )?,
            available_at: SqliteRepository::field(row, "available_at")?,
            lease_owner: SqliteRepository::field(row, "lease_owner")?,
            lease_expires_at: SqliteRepository::field(row, "lease_expires_at")?,
            last_error: SqliteRepository::field(row, "last_error")?,
            created_at: SqliteRepository::field(row, "created_at")?,
            delivered_at: SqliteRepository::field(row, "delivered_at")?,
        })
    }
}

#[cfg(test)]
#[path = "../../../tests/storage/outbox/sqlite_test.rs"]
mod tests;

use crate::utils::storage::PaymentStorageCodec;

use crate::storage::outbox::OutboxStorageRules;
