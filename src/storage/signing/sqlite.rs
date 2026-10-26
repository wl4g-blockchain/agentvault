use crate::storage::{
    payment::sqlite::PaymentSqliteRepository as PaymentRepository,
    policy::sqlite::PolicySqliteRepository as PolicyRepository, sqlite::SqliteRepository as BaseRepository,
    wallet::sqlite::WalletSqliteRepository as WalletRepository,
};
use sqlx::SqliteConnection as Connection;

// Signing repository; transaction rules shared across SQLx drivers.

crate::storage::signing::transactions::signing_repository!(SigningSqliteRepository, "", "", "", "", "", "", "");

#[cfg(test)]
#[path = "../../../tests/storage/signing/sqlite_test.rs"]
mod tests;

#[derive(Clone)]
pub struct SigningSqliteRepository {
    base: BaseRepository,
}
impl SigningSqliteRepository {
    pub(crate) fn new(base: BaseRepository) -> Self {
        Self { base }
    }
}
