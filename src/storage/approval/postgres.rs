use crate::{
    storage::{
        event::postgres::EventPostgresRepository as EventRepository,
        outbox::postgres::OutboxPostgresRepository as OutboxRepository,
        payment::postgres::PaymentPostgresRepository as PaymentRepository,
        postgres::{dynamic_postgres as dynamic_query, BasePostgresRepository as BaseRepository},
    },
    utils::storage::PostgresAmountCodec as AmountCodec,
};
use sqlx::{postgres::PgRow as DbRow, PgConnection as Connection};

// Approval repository; transaction rules shared across SQLx drivers.

type AmountValue = crate::utils::storage::PgAmount;
type JsonValue = serde_json::Value;

impl ApprovalPostgresRepository {
    async fn lock_approval_intent(connection: &mut Connection, intent_id: &str) -> Result<()> {
        let lock_key = format!("agentvault:approval:{intent_id}");
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_key)
            .execute(connection)
            .await
            .map_err(|error| BaseRepository::storage_error("lock approval intent", error))?;
        Ok(())
    }
}

crate::storage::approval::transactions::approval_repository!(
    ApprovalPostgresRepository,
    " FOR UPDATE",
    " FOR SHARE",
    " FOR UPDATE SKIP LOCKED",
    " FOR UPDATE OF i",
    " FOR SHARE OF c",
    " FOR SHARE OF policy",
    " FOR SHARE OF c, s"
);

#[cfg(test)]
#[path = "../../../tests/storage/approval/postgres_test.rs"]
mod tests;

#[derive(Clone)]
pub struct ApprovalPostgresRepository {
    base: BaseRepository,
}
impl ApprovalPostgresRepository {
    pub(crate) fn new(base: BaseRepository) -> Self {
        Self { base }
    }
}
