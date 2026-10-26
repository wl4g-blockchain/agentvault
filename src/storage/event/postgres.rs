//! Event persistence for `postgres`.

use crate::{
    model::{PaymentEvent, PaymentEventType},
    storage::{postgres::BasePostgresRepository, EventStorage, IdempotentWrite, Page, PageRequest},
    Error, Result,
};
use async_trait::async_trait;
use sqlx::{postgres::PgRow, PgConnection};

#[derive(Clone)]
pub struct EventPostgresRepository {
    base: BasePostgresRepository,
}

#[async_trait]
impl EventStorage for EventPostgresRepository {
    async fn append(&self, event: &PaymentEvent) -> Result<IdempotentWrite<PaymentEvent>> {
        let event = event.clone();
        self.base
            .transaction(|connection| Box::pin(async move { Self::insert_event(connection, &event).await }))
            .await
    }

    async fn get(&self, event_id: &str) -> Result<Option<PaymentEvent>> {
        self.base
            .select_one(crate::storage::EntityQuery::new().eq("id", event_id))
            .await
    }

    async fn find_for_intent_type(
        &self,
        intent_id: &str,
        event_type: PaymentEventType,
        attempt_no: u32,
    ) -> Result<Option<PaymentEvent>> {
        self.base
            .select_one(
                crate::storage::EntityQuery::new()
                    .eq("intent_id", intent_id)
                    .eq("event_type", event_type.as_str())
                    .eq("attempt_no", i64::from(attempt_no)),
            )
            .await
    }

    async fn list_for_intent(&self, intent_id: &str, page: PageRequest) -> Result<Page<PaymentEvent>> {
        self.base
            .entity_page(
                crate::storage::EntityQuery::new()
                    .eq("intent_id", intent_id)
                    .order("created_at", false)
                    .order("id", false),
                page,
            )
            .await
    }
}

impl EventPostgresRepository {
    pub(crate) fn new(base: BasePostgresRepository) -> Self {
        Self { base }
    }

    pub(crate) async fn insert_event(
        connection: &mut PgConnection,
        event: &PaymentEvent,
    ) -> Result<IdempotentWrite<PaymentEvent>> {
        event
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid payment event {field}")))?;

        let inserted = crate::storage::postgres::dynamic_postgres!(insert_or_ignore, event)?
            .push(" RETURNING *")
            .build()
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| crate::Error::conflict(format!("payment event append rejected: {error}")))?;
        if let Some(row) = inserted {
            return Self::decode_row(&row).map(IdempotentWrite::Inserted);
        }
        let existing = sqlx::query(
            "SELECT * FROM pay_event WHERE id = $1 OR (intent_id = $2 AND event_type = $3 AND attempt_no = $4) \
     FOR SHARE",
        )
        .bind(&event.id)
        .bind(&event.intent_id)
        .bind(event.event_type.as_str())
        .bind(i64::from(event.attempt_no))
        .fetch_all(&mut *connection)
        .await
        .map_err(|error| BasePostgresRepository::storage_error("find payment event replay", error))?;
        if existing.len() != 1 {
            return Err(crate::Error::conflict(
                "payment event identifiers collide with multiple rows",
            ));
        }
        let existing = Self::decode_row(&existing[0])?;
        if existing == *event {
            Ok(IdempotentWrite::Replay(existing))
        } else {
            Err(crate::Error::conflict(
                "payment event idempotency identity has different content",
            ))
        }
    }

    pub(crate) fn decode_row(row: &PgRow) -> Result<PaymentEvent> {
        sqlx::FromRow::from_row(row)
            .map_err(|error| BasePostgresRepository::storage_error("decode event entity", error))
    }
}

#[cfg(test)]
#[path = "../../../tests/storage/event/postgres_test.rs"]
mod tests;
