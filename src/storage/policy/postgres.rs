use crate::{
    storage::postgres::{dynamic_postgres as dynamic_query, BasePostgresRepository as BaseRepository},
    utils::storage::PostgresAmountCodec as AmountCodec,
};
use sqlx::{postgres::PgRow as DbRow, PgConnection as Connection, PgPool as Pool};

// Policy repository; transaction rules shared across SQLx drivers.

type AmountValue = crate::utils::storage::PgAmount;
type JsonValue = serde_json::Value;

impl PolicyPostgresRepository {
    async fn lock_wallets(connection: &mut Connection, wallet_ids: &[String]) -> Result<()> {
        let locked: Vec<String> = sqlx::query_scalar(concat!(
            "SELECT id FROM pay_wallet WHERE id = ANY($1) ORDER BY id",
            " FOR UPDATE"
        ))
        .bind(wallet_ids)
        .fetch_all(connection)
        .await
        .map_err(|error| BaseRepository::storage_error("lock payment policy wallets", error))?;
        if locked != wallet_ids {
            return Err(crate::Error::conflict(
                "payment policy references an unavailable wallet",
            ));
        }
        Ok(())
    }
}

crate::storage::policy::transactions::policy_repository!(
    PolicyPostgresRepository,
    " FOR UPDATE",
    " FOR SHARE",
    " FOR UPDATE SKIP LOCKED",
    " FOR UPDATE OF i",
    " FOR SHARE OF c",
    " FOR SHARE OF policy",
    " FOR SHARE OF c, s"
);

#[cfg(test)]
#[path = "../../../tests/storage/policy/postgres_test.rs"]
mod tests;

#[derive(Clone)]
pub struct PolicyPostgresRepository {
    base: BaseRepository,
}
impl PolicyPostgresRepository {
    pub(crate) fn new(base: BaseRepository) -> Self {
        Self { base }
    }
}
