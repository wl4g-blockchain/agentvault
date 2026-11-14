use crate::{
    storage::base_sqlite::SqliteRepository as BaseRepository,
    storage::payment::sqlite::PaymentSqliteRepository as PaymentRepository,
    storage::policy::sqlite::PolicySqliteRepository as PolicyRepository,
    storage::wallet::sqlite::WalletSqliteRepository as WalletRepository,
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
