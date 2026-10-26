use crate::{
    storage::sqlite::{dynamic_sqlite as dynamic_query, BaseSqliteRepository as BaseRepository},
    utils::storage::SqliteAmountCodec as AmountCodec,
};
use sqlx::{sqlite::SqliteRow as DbRow, SqliteConnection as Connection, SqlitePool as Pool};

// Policy repository; transaction rules shared across SQLx drivers.

type AmountValue = String;
type JsonValue = String;

impl PolicySqliteRepository {
    async fn lock_wallets(connection: &mut Connection, wallet_ids: &[String]) -> Result<()> {
        for wallet_id in wallet_ids {
            let found: Option<String> = sqlx::query_scalar("SELECT id FROM pay_wallet WHERE id = $1")
                .bind(wallet_id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|e| BaseRepository::storage_error("check policy wallet", e))?;
            if found.is_none() {
                return Err(Error::conflict("payment policy references an unavailable wallet"));
            }
        }
        Ok(())
    }
}

crate::storage::policy::transactions::policy_repository!(PolicySqliteRepository, "", "", "", "", "", "", "");

#[cfg(test)]
#[path = "../../../tests/storage/policy/sqlite_test.rs"]
mod tests;

#[derive(Clone)]
pub struct PolicySqliteRepository {
    base: BaseRepository,
}
impl PolicySqliteRepository {
    pub(crate) fn new(base: BaseRepository) -> Self {
        Self { base }
    }
}
