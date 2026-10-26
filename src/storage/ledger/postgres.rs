//! Ledger persistence for `postgres`.

use crate::{
    model::PaymentLedgerEntry,
    storage::{
        outbox::postgres::OutboxPostgresRepository, postgres::PostgresRepository, ILedgerRepository, IdempotentWrite,
        LedgerQuery, LedgerWrite, Page,
    },
    Error, Result,
};
use async_trait::async_trait;
use sqlx::{postgres::PgRow, PgConnection};
use std::collections::BTreeSet;

#[derive(Clone)]
pub struct LedgerPostgresRepository {
    base: PostgresRepository,
}

#[async_trait]
impl ILedgerRepository for LedgerPostgresRepository {
    async fn append(&self, entry: &PaymentLedgerEntry) -> Result<IdempotentWrite<PaymentLedgerEntry>> {
        let entry = entry.clone();
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    Self::lock_ledger_scopes(connection, std::slice::from_ref(&entry)).await?;
                    Self::insert_ledger(connection, &entry).await
                })
            })
            .await
    }

    async fn append_batch(&self, write: LedgerWrite<'_>) -> Result<Vec<IdempotentWrite<PaymentLedgerEntry>>> {
        let entries = write.entries.to_vec();
        let outbox = write.outbox.to_vec();
        self.base
            .transaction(|connection| {
                Box::pin(async move {
                    Self::lock_ledger_scopes(connection, &entries).await?;
                    let mut results = Vec::with_capacity(entries.len());
                    for entry in &entries {
                        results.push(Self::insert_ledger(connection, entry).await?);
                    }
                    for message in &outbox {
                        OutboxPostgresRepository::insert_outbox(connection, message).await?;
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

impl LedgerPostgresRepository {
    pub(crate) fn new(base: PostgresRepository) -> Self {
        Self { base }
    }

    pub(crate) async fn lock_financial_scopes(
        connection: &mut PgConnection,
        scenario_ids: &[String],
        wallet_ids: &[String],
    ) -> Result<()> {
        let scenario_ids = scenario_ids.iter().cloned().collect::<BTreeSet<_>>();
        let wallet_ids = wallet_ids.iter().cloned().collect::<BTreeSet<_>>();
        let scenario_ids = scenario_ids.into_iter().collect::<Vec<_>>();
        let wallet_ids = wallet_ids.into_iter().collect::<Vec<_>>();
        if !scenario_ids.is_empty() {
            let locked: Vec<String> =
                sqlx::query_scalar("SELECT id FROM pay_scenario WHERE id = ANY($1) ORDER BY id FOR UPDATE")
                    .bind(&scenario_ids)
                    .fetch_all(&mut *connection)
                    .await
                    .map_err(|error| PostgresRepository::storage_error("lock ledger scenarios", error))?;
            if locked.len() != scenario_ids.len() {
                return Err(crate::Error::conflict("ledger references an unavailable scenario"));
            }
        }
        if !wallet_ids.is_empty() {
            let locked: Vec<String> =
                sqlx::query_scalar("SELECT id FROM pay_wallet WHERE id = ANY($1) ORDER BY id FOR UPDATE")
                    .bind(&wallet_ids)
                    .fetch_all(connection)
                    .await
                    .map_err(|error| PostgresRepository::storage_error("lock ledger wallets", error))?;
            if locked.len() != wallet_ids.len() {
                return Err(crate::Error::conflict("ledger references an unavailable wallet"));
            }
        }
        Ok(())
    }

    async fn lock_ledger_scopes(connection: &mut PgConnection, entries: &[PaymentLedgerEntry]) -> Result<()> {
        let scenario_ids = entries
            .iter()
            .filter_map(|entry| entry.scenario_id.clone())
            .collect::<Vec<_>>();
        let wallet_ids = entries.iter().map(|entry| entry.wallet_id.clone()).collect::<Vec<_>>();
        Self::lock_financial_scopes(connection, &scenario_ids, &wallet_ids).await
    }

    pub(crate) async fn insert_ledger(
        connection: &mut PgConnection,
        entry: &PaymentLedgerEntry,
    ) -> Result<IdempotentWrite<PaymentLedgerEntry>> {
        entry
            .validate()
            .map_err(|field| Error::InvalidRequest(format!("invalid ledger entry {field}")))?;
        if let Some(original_id) = &entry.reversal_of {
            let row = sqlx::query("SELECT * FROM pay_ledger WHERE id = $1 FOR UPDATE")
                .bind(original_id)
                .fetch_optional(&mut *connection)
                .await
                .map_err(|error| PostgresRepository::storage_error("lock ledger reversal original", error))?
                .ok_or_else(|| crate::Error::conflict("ledger reversal original does not exist"))?;
            let original = Self::decode_row(&row)?;
            entry
                .validate_reversal_of(&original)
                .map_err(|_| crate::Error::conflict("ledger reversal does not exactly negate its original"))?;
        }
        let inserted = crate::storage::postgres::dynamic_postgres!(insert_or_ignore, entry)?
            .push(" RETURNING *")
            .build()
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| crate::Error::conflict(format!("ledger append rejected: {error}")))?;
        if let Some(row) = inserted {
            return Self::decode_row(&row).map(IdempotentWrite::Inserted);
        }
        let row = sqlx::query("SELECT * FROM pay_ledger WHERE wallet_id = $1 AND posting_key = $2 FOR SHARE")
            .bind(&entry.wallet_id)
            .bind(&entry.posting_key)
            .fetch_optional(&mut *connection)
            .await
            .map_err(|error| PostgresRepository::storage_error("find ledger posting replay", error))?;
        let Some(row) = row else {
            return Err(crate::Error::conflict(
                "ledger identity or reversal fact conflicts with an existing row",
            ));
        };
        let existing = Self::decode_row(&row)?;
        if existing.same_fact_content_as(entry) {
            Ok(IdempotentWrite::Replay(existing))
        } else {
            Err(crate::Error::conflict(
                "ledger posting key is already bound to different financial content",
            ))
        }
    }

    pub(crate) fn decode_row(row: &PgRow) -> Result<PaymentLedgerEntry> {
        sqlx::FromRow::from_row(row).map_err(|error| PostgresRepository::storage_error("decode ledger entity", error))
    }
}

#[cfg(test)]
#[path = "../../../tests/storage/ledger/postgres_test.rs"]
mod tests;
