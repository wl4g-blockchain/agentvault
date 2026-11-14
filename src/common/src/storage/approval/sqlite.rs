use crate::{
    storage::base_sqlite::dynamic_sqlite as dynamic_query, storage::base_sqlite::SqliteRepository as BaseRepository,
    storage::event::sqlite::EventSqliteRepository as EventRepository,
    storage::outbox::sqlite::OutboxSqliteRepository as OutboxRepository,
    storage::payment::sqlite::PaymentSqliteRepository as PaymentRepository,
    util::storage::SqliteAmountCodec as AmountCodec,
};
use sqlx::{sqlite::SqliteRow as DbRow, SqliteConnection as Connection};

// Approval repository; transaction rules shared across SQLx drivers.

type AmountValue = String;
type JsonValue = String;

impl ApprovalSqliteRepository {
    fn lock_approval_intent(_connection: &mut Connection, _intent_id: &str) -> std::future::Ready<Result<()>> {
        // BEGIN IMMEDIATE already owns the SQLite database write lock.
        std::future::ready(Ok(()))
    }
}

crate::storage::approval::transactions::approval_repository!(ApprovalSqliteRepository, "", "", "", "", "", "", "");

#[cfg(test)]
#[path = "../../../tests/storage/approval/sqlite_test.rs"]
mod tests;

#[derive(Clone)]
pub struct ApprovalSqliteRepository {
    base: BaseRepository,
}
impl ApprovalSqliteRepository {
    pub(crate) fn new(base: BaseRepository) -> Self {
        Self { base }
    }
}
