use crate::{
    storage::codec::SqliteAmountCodec as AmountCodec, storage::sqlite::dynamic_sqlite as dynamic_query,
    storage::sqlite::SqliteRepository as BaseRepository,
};
use sqlx::{sqlite::SqliteRow as DbRow, SqliteConnection as Connection};

// Wallet repository; transaction rules shared across SQLx drivers.

type AmountValue = String;

crate::storage::wallet::r#macro::wallet_repository!(WalletSqliteRepository, "", "", "", "", "", "", "");
pub type SqliteWalletKeyRepository = crate::storage::wallet::WalletKeyRepository<WalletSqliteRepository>;
impl From<BaseRepository> for WalletSqliteRepository {
    fn from(base: BaseRepository) -> Self {
        Self::new(base)
    }
}

#[cfg(test)]
#[path = "../../../../tests/storage/wallet/sqlite_test.rs"]
mod tests;

#[derive(Clone)]
pub struct WalletSqliteRepository {
    base: BaseRepository,
}
impl WalletSqliteRepository {
    pub(crate) fn new(base: BaseRepository) -> Self {
        Self { base }
    }
}
