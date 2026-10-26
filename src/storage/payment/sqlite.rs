use crate::{
    storage::{
        approval::sqlite::ApprovalSqliteRepository as ApprovalRepository,
        event::sqlite::EventSqliteRepository as EventRepository,
        ledger::sqlite::LedgerSqliteRepository as LedgerRepository,
        outbox::sqlite::OutboxSqliteRepository as OutboxRepository,
        policy::sqlite::PolicySqliteRepository as PolicyRepository,
        scenario::sqlite::ScenarioSqliteRepository as ScenarioRepository,
        sqlite::{dynamic_sqlite as dynamic_query, BaseSqliteRepository as BaseRepository},
        wallet::sqlite::WalletSqliteRepository as WalletRepository,
    },
    utils::storage::SqliteAmountCodec as AmountCodec,
};
use sqlx::{sqlite::SqliteRow as DbRow, SqliteConnection as Connection, SqlitePool as Pool};

// Payment repository; transaction rules shared across SQLx drivers.

impl PaymentSqliteRepository {
    fn lock_run_id(_connection: &mut Connection, _scenario_id: &str, _run_id: &str) -> std::future::Ready<Result<()>> {
        // BEGIN IMMEDIATE already owns the SQLite database write lock.
        std::future::ready(Ok(()))
    }
}

crate::storage::payment::transactions::payment_repository!(PaymentSqliteRepository, "", "", "", "", "", "", "");

#[cfg(test)]
#[path = "../../../tests/storage/payment/sqlite_test.rs"]
mod tests;

#[derive(Clone)]
pub struct PaymentSqliteRepository {
    base: BaseRepository,
}
impl PaymentSqliteRepository {
    pub(crate) fn new(base: BaseRepository) -> Self {
        Self { base }
    }
}
