use crate::{
    storage::postgres::{dynamic_postgres as dynamic_query, PostgresRepository as BaseRepository},
    utils::storage::PostgresAmountCodec as AmountCodec,
};
use sqlx::{postgres::PgRow as DbRow, PgConnection as Connection};

// Wallet repository; transaction rules shared across SQLx drivers.

type AmountValue = crate::utils::storage::PgAmount;

crate::storage::wallet::transactions::wallet_repository!(
    WalletPostgresRepository,
    " FOR UPDATE",
    " FOR SHARE",
    " FOR UPDATE SKIP LOCKED",
    " FOR UPDATE OF i",
    " FOR SHARE OF c",
    " FOR SHARE OF policy",
    " FOR SHARE OF c, s"
);
pub type PgWalletKeyRepository = crate::storage::wallet::key::WalletKeyRepository<WalletPostgresRepository>;
impl From<BaseRepository> for WalletPostgresRepository {
    fn from(base: BaseRepository) -> Self {
        Self::new(base)
    }
}

#[cfg(test)]
#[path = "../../../tests/storage/wallet/postgres_test.rs"]
mod tests;

#[derive(Clone)]
pub struct WalletPostgresRepository {
    base: BaseRepository,
}
impl WalletPostgresRepository {
    pub(crate) fn new(base: BaseRepository) -> Self {
        Self { base }
    }
}
