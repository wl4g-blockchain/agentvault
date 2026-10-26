//! Ledger persistence for `sqlite`.

use crate::{
    model::PaymentLedgerEntry,
    storage::{
        outbox::sqlite::OutboxSqliteRepository, sqlite::SqliteRepository, ILedgerRepository, IdempotentWrite,
        LedgerQuery, LedgerWrite, Page,
    },
    Error, Result,
};
use async_trait::async_trait;
use sqlx::sqlite::SqliteRow;

#[derive(Clone)]
pub struct LedgerSqliteRepository {
    base: SqliteRepository,
}

#[async_trait]
impl ILedgerRepository for LedgerSqliteRepository {
    async fn append(&self, entry: &PaymentLedgerEntry) -> Result<IdempotentWrite<PaymentLedgerEntry>> {
        let entry = entry.clone();
        self.base
            .immediate(|connection| Box::pin(async move { Self::insert_ledger(connection, &entry).await }))
            .await
    }

    async fn append_batch(&self, write: LedgerWrite<'_>) -> Result<Vec<IdempotentWrite<PaymentLedgerEntry>>> {
        let entries = write.entries.to_vec();
        let outbox = write.outbox.to_vec();
        self.base
            .immediate(|connection| {
                Box::pin(async move {
                    let mut results = Vec::with_capacity(entries.len());
                    for entry in &entries {
                        results.push(Self::insert_ledger(connection, entry).await?);
                    }
                    for message in &outbox {
                        OutboxSqliteRepository::insert_outbox(connection, message).await?;
                    }
                    Ok(results)
                })
            })
            .await
    }

    async fn get(&self, ledger_id: &str) -> Result<Option<PaymentLedgerEntry>> {
        self.base
            .select_one(crate::storage::EntityQuery::new().eq("id", ledger_id))
            .await
    }

    async fn find_posting(&self, wallet_id: &str, posting_key: &str) -> Result<Option<PaymentLedgerEntry>> {
        self.base
            .select_one(
                crate::storage::EntityQuery::new()
                    .eq("wallet_id", wallet_id)
                    .eq("posting_key", posting_key),
            )
            .await
    }

    async fn list(&self, query: &LedgerQuery) -> Result<Page<PaymentLedgerEntry>> {
        self.base
            .entity_page(
                query.entity_query().order("effective_at", true).order("id", false),
                query.page,
            )
            .await
    }
}

impl LedgerSqliteRepository {
    pub(crate) async fn lock_financial_scopes(
        connection: &mut sqlx::SqliteConnection,
        scenario_ids: &[String],
        wallet_ids: &[String],
    ) -> Result<()> {
        // The enclosing IMMEDIATE transaction holds SQLite's writer reservation.
        for (table, ids) in [("pay_scenario", scenario_ids), ("pay_wallet", wallet_ids)] {
            for id in ids {
                let found: Option<String> = sqlx::query_scalar(&format!("SELECT id FROM {table} WHERE id = $1"))
                    .bind(id)
                    .fetch_optional(&mut *connection)
                    .await
                    .map_err(|e| SqliteRepository::storage_error("check financial scope", e))?;
                if found.is_none() {
                    return Err(Error::conflict("ledger references an unavailable scope"));
                }
            }
        }
        Ok(())
    }
    pub(crate) fn new(base: SqliteRepository) -> Self {
        Self { base }
    }

    pub(crate) async fn insert_ledger(
        connection: &mut sqlx::SqliteConnection,
        entry: &PaymentLedgerEntry,
    ) -> Result<IdempotentWrite<PaymentLedgerEntry>> {
        entry
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid ledger entry {field}")))?;
        let existing = crate::storage::sqlite::dynamic_sqlite!(
            select,
            crate::model::PaymentLedgerEntry,
            crate::storage::EntityQuery::new()
                .eq("wallet_id", &entry.wallet_id)
                .eq("posting_key", &entry.posting_key)
        )?
        .build()
        .fetch_optional(&mut *connection)
        .await
        .map_err(|error| SqliteRepository::storage_error("find ledger posting replay", error))?;
        if let Some(row) = existing {
            let existing = Self::decode_row(&row)?;
            return if existing.same_fact_content_as(entry) {
                Ok(IdempotentWrite::Replay(existing))
            } else {
                Err(crate::Error::conflict(
                    "ledger posting key is already bound to different financial content",
                ))
            };
        }
        if let Some(original_id) = &entry.reversal_of {
            let row = crate::storage::sqlite::dynamic_sqlite!(
                select,
                crate::model::PaymentLedgerEntry,
                crate::storage::EntityQuery::new().eq("id", original_id)
            )?
            .build()
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| SqliteRepository::storage_error("load ledger reversal original", error))?
            .ok_or_else(|| crate::Error::conflict("ledger reversal original does not exist"))?;
            let original = Self::decode_row(&row)?;
            entry
                .validate_reversal_of(&original)
                .map_err(|_| crate::Error::conflict("ledger reversal does not exactly negate its original"))?;
        }
        crate::storage::sqlite::dynamic_sqlite!(insert, entry)?
            .build()
            .execute(connection)
            .await
            .map_err(|error| crate::Error::conflict(format!("ledger append rejected: {error}")))?;
        Ok(IdempotentWrite::Inserted(entry.clone()))
    }

    pub(crate) fn decode_row(row: &SqliteRow) -> Result<PaymentLedgerEntry> {
        sqlx::FromRow::from_row(row).map_err(|error| SqliteRepository::storage_error("decode ledger entity", error))
    }
}

#[cfg(test)]
#[path = "../../../tests/storage/ledger/sqlite_test.rs"]
mod tests;
