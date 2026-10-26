//! Outbox persistence for `postgres`.

use crate::{
    storage::{postgres::PostgresRepository, IOutboxRepository, OutboxLeaseRequest, OutboxMessage},
    Error, Result,
};
use async_trait::async_trait;
use sqlx::{postgres::PgRow, PgConnection};

#[derive(Clone)]
pub struct OutboxPostgresRepository {
    base: PostgresRepository,
}

#[async_trait]
impl IOutboxRepository for OutboxPostgresRepository {
    async fn claim(&self, request: &OutboxLeaseRequest) -> Result<Vec<OutboxMessage>> {
        if request.owner.trim().is_empty() || request.lease_until <= request.now || request.limit == 0 {
            return Err(Error::InvalidRequest("invalid outbox lease request".to_owned()));
        }
        let request = request.clone();
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    let rows = sqlx::query(
                        "WITH due AS ( \
                         SELECT id FROM pay_outbox WHERE status = 'pending' AND available_at <= $1 \
                         AND (lease_expires_at IS NULL OR lease_expires_at <= $1) \
                         AND (($5 = 'approval_required' AND event_type = 'approval_required' \
                               AND destination LIKE 'client:_%.approvals') \
                              OR ($5 = 'payment_events' AND (destination LIKE 'client:_%.payments' \
                                  OR destination LIKE 'scenario:_%.payments'))) \
                         AND attempt_count < 4294967295 ORDER BY available_at, created_at, id \
                         FOR UPDATE SKIP LOCKED LIMIT $2 \
                     ) \
                     UPDATE pay_outbox message SET lease_owner = $3, lease_expires_at = $4, \
                         attempt_count = message.attempt_count + 1 FROM due WHERE message.id = due.id \
                     RETURNING message.*",
                    )
                    .bind(request.now)
                    .bind(i64::from(request.limit))
                    .bind(&request.owner)
                    .bind(request.lease_until)
                    .bind(request.class.as_str())
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(|error| PostgresRepository::storage_error("claim due outbox messages", error))?;
                    let mut messages = rows.iter().map(Self::decode_row).collect::<Result<Vec<_>>>()?;
                    messages.sort_by(|left, right| {
                        (left.available_at, left.created_at, left.id.as_str()).cmp(&(
                            right.available_at,
                            right.created_at,
                            right.id.as_str(),
                        ))
                    });
                    Ok(messages)
                })
            })
            .await
    }

    async fn mark_delivered(&self, message_id: &str, owner: &str, delivered_at: i64) -> Result<()> {
        Self::transition_lease(
            self,
            "UPDATE pay_outbox SET status = 'delivered', delivered_at = $1, lease_owner = NULL, \
             lease_expires_at = NULL, last_error = NULL WHERE id = $2 AND status = 'pending' AND lease_owner = $3",
            message_id,
            owner,
            delivered_at,
            "mark outbox message delivered",
        )
        .await
    }

    async fn reschedule(&self, message_id: &str, owner: &str, available_at: i64, error: &str) -> Result<()> {
        Self::validate_lease_identity(message_id, owner)?;
        let result = sqlx::query(
            "UPDATE pay_outbox SET available_at = $1, lease_owner = NULL, lease_expires_at = NULL, \
             last_error = $4 WHERE id = $2 AND status = 'pending' AND lease_owner = $3",
        )
        .bind(available_at)
        .bind(message_id)
        .bind(owner)
        .bind(error)
        .execute(self.base.pool())
        .await
        .map_err(|source| PostgresRepository::storage_error("reschedule outbox message", source))?;
        Self::ensure_transitioned(result.rows_affected())
    }

    async fn mark_dead_letter(&self, message_id: &str, owner: &str, error: &str) -> Result<()> {
        Self::validate_lease_identity(message_id, owner)?;
        let result = sqlx::query(
            "UPDATE pay_outbox SET status = 'dead_letter', lease_owner = NULL, lease_expires_at = NULL, \
             last_error = $3 WHERE id = $1 AND status = 'pending' AND lease_owner = $2",
        )
        .bind(message_id)
        .bind(owner)
        .bind(error)
        .execute(self.base.pool())
        .await
        .map_err(|source| PostgresRepository::storage_error("mark outbox message dead-letter", source))?;
        Self::ensure_transitioned(result.rows_affected())
    }
}

impl OutboxPostgresRepository {
    pub(crate) fn new(base: PostgresRepository) -> Self {
        Self { base }
    }

    pub(crate) async fn insert_outbox(connection: &mut PgConnection, message: &OutboxMessage) -> Result<()> {
        OutboxStorageRules::validate_message(message)?;
        let payload = PostgresRepository::encode_json(&message.payload, "pay_outbox.payload")?;
        let inserted = sqlx::query(
            "INSERT INTO pay_outbox (id, aggregate_type, aggregate_id, event_type, destination, \
     run_id, payload, status, attempt_count, available_at, lease_owner, lease_expires_at, \
     last_error, created_at, delivered_at) \
     VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15) \
     ON CONFLICT DO NOTHING RETURNING id",
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
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| crate::Error::conflict(format!("outbox insert rejected: {error}")))?;
        if inserted.is_some() {
            return Ok(());
        }
        let existing = sqlx::query(
    "SELECT * FROM pay_outbox WHERE id = $1 OR (destination = $2 AND event_type = $3 AND run_id = $4) FOR SHARE",
)
.bind(&message.id)
.bind(&message.destination)
.bind(&message.event_type)
.bind(&message.run_id)
.fetch_all(&mut *connection)
.await
.map_err(|error| PostgresRepository::storage_error("find outbox replay", error))?;
        if existing.len() == 1 && OutboxStorageRules::same_logical_message(&Self::decode_row(&existing[0])?, message) {
            Ok(())
        } else {
            Err(crate::Error::conflict("outbox run identity has different content"))
        }
    }

    async fn transition_lease(
        storage: &OutboxPostgresRepository,
        sql: &'static str,
        message_id: &str,
        owner: &str,
        timestamp: i64,
        operation: &'static str,
    ) -> Result<()> {
        Self::validate_lease_identity(message_id, owner)?;
        let result = sqlx::query(sql)
            .bind(timestamp)
            .bind(message_id)
            .bind(owner)
            .execute(storage.base.pool())
            .await
            .map_err(|source| PostgresRepository::storage_error(operation, source))?;
        Self::ensure_transitioned(result.rows_affected())
    }

    fn validate_lease_identity(message_id: &str, owner: &str) -> Result<()> {
        if message_id.trim().is_empty() || owner.trim().is_empty() {
            return Err(Error::InvalidRequest("outbox lease identity is incomplete".to_owned()));
        }
        Ok(())
    }

    fn ensure_transitioned(rows_affected: u64) -> Result<()> {
        if rows_affected == 1 {
            Ok(())
        } else {
            Err(crate::Error::conflict("outbox message is not held by the lease owner"))
        }
    }

    pub(crate) fn decode_row(row: &PgRow) -> Result<OutboxMessage> {
        Ok(OutboxMessage {
            id: PostgresRepository::field(row, "id")?,
            aggregate_type: PostgresRepository::field(row, "aggregate_type")?,
            aggregate_id: PostgresRepository::field(row, "aggregate_id")?,
            event_type: PostgresRepository::field(row, "event_type")?,
            destination: PostgresRepository::field(row, "destination")?,
            run_id: PostgresRepository::field(row, "run_id")?,
            payload: PostgresRepository::decode_json(PostgresRepository::field(row, "payload")?, "pay_outbox.payload")?,
            status: PaymentStorageCodec::outbox_status(&PostgresRepository::field::<String>(row, "status")?)?,
            attempt_count: PostgresRepository::decode_u32(
                PostgresRepository::field(row, "attempt_count")?,
                "pay_outbox.attempt_count",
            )?,
            available_at: PostgresRepository::field(row, "available_at")?,
            lease_owner: PostgresRepository::field(row, "lease_owner")?,
            lease_expires_at: PostgresRepository::field(row, "lease_expires_at")?,
            last_error: PostgresRepository::field(row, "last_error")?,
            created_at: PostgresRepository::field(row, "created_at")?,
            delivered_at: PostgresRepository::field(row, "delivered_at")?,
        })
    }
}

#[cfg(test)]
#[path = "../../../tests/storage/outbox/postgres_test.rs"]
mod tests;

use crate::utils::storage::PaymentStorageCodec;

use crate::storage::outbox::OutboxStorageRules;
