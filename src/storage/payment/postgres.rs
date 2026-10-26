use crate::{
    storage::{
        approval::postgres::ApprovalPostgresRepository as ApprovalRepository,
        event::postgres::EventPostgresRepository as EventRepository,
        ledger::postgres::LedgerPostgresRepository as LedgerRepository,
        outbox::postgres::OutboxPostgresRepository as OutboxRepository,
        policy::postgres::PolicyPostgresRepository as PolicyRepository,
        postgres::{dynamic_postgres as dynamic_query, BasePostgresRepository as BaseRepository},
        scenario::postgres::ScenarioPostgresRepository as ScenarioRepository,
        wallet::postgres::WalletPostgresRepository as WalletRepository,
    },
    utils::storage::PostgresAmountCodec as AmountCodec,
};
use sqlx::{postgres::PgRow as DbRow, PgConnection as Connection, PgPool as Pool};

// Payment repository; transaction rules shared across SQLx drivers.

impl PaymentPostgresRepository {
    async fn lock_run_id(connection: &mut Connection, scenario_id: &str, run_id: &str) -> Result<()> {
        let lock_key = format!("agentvault:payment:{scenario_id}:{run_id}");
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(lock_key)
            .execute(connection)
            .await
            .map_err(|error| BaseRepository::storage_error("lock payment run", error))?;
        Ok(())
    }
}

crate::storage::payment::transactions::payment_repository!(
    PaymentPostgresRepository,
    " FOR UPDATE",
    " FOR SHARE",
    " FOR UPDATE SKIP LOCKED",
    " FOR UPDATE OF i",
    " FOR SHARE OF c",
    " FOR SHARE OF policy",
    " FOR SHARE OF c, s"
);

#[cfg(test)]
#[path = "../../../tests/storage/payment/postgres_test.rs"]
mod tests;

#[derive(Clone)]
pub struct PaymentPostgresRepository {
    base: BaseRepository,
}
impl PaymentPostgresRepository {
    pub(crate) fn new(base: BaseRepository) -> Self {
        Self { base }
    }
}
