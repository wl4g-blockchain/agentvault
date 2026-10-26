use std::{env, sync::Arc};

use rusqlite::{params, Connection};
use serde_json::Value;
use tokio_cron_scheduler::{Job, JobScheduler};

use crate::agent::mcp::{DynError, MCPHelper, ToolResult};

#[derive(Clone)]
pub struct IngestionJob {
    mcp: MCPHelper,
    database: String,
    yesterday_cron: String,
    realtime_cron: String,
    fail_after_settlement_once: bool,
}

impl IngestionJob {
    /// Builds the two scheduled ingestion jobs and durable evidence store.
    ///
    /// # Errors
    ///
    /// Returns an error for missing configuration, an in-memory evidence
    /// database, or invalid MCP payer configuration.
    pub async fn from_env() -> Result<Self, DynError> {
        let database = required_env("QUANTSTREAM_DATABASE")?;
        if database == ":memory:" {
            return Err("QUANTSTREAM_DATABASE must be persistent".into());
        }
        Ok(Self {
            mcp: MCPHelper::from_env().await?,
            database,
            yesterday_cron: optional_env("QUANTSTREAM_YESTERDAY_CRON", "0 0 2 * * *"),
            realtime_cron: optional_env("QUANTSTREAM_REALTIME_CRON", "0 */1 * * * *"),
            fail_after_settlement_once: env::var("QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT")
                .is_ok_and(|value| value == "once"),
        })
    }

    /// Scheduled delayed-news ingestion; this tool is deliberately free.
    ///
    /// # Errors
    ///
    /// Returns an error when the `MarketPulse` MCP call fails validation or
    /// transport.
    pub async fn ingest_yesterday_news(&self, run_id: &str) -> Result<ToolResult, DynError> {
        self.mcp.yesterday_news(run_id).await
    }

    /// Scheduled realtime-news ingestion; the SDK transparently pays x402.
    ///
    /// # Errors
    ///
    /// Returns an error when payment authorization, delivery, settlement
    /// evidence reporting, or MCP validation fails.
    pub async fn ingest_realtime_news(&self, run_id: &str, inject_failure: bool) -> Result<ToolResult, DynError> {
        self.mcp.realtime_news(run_id, inject_failure).await
    }

    /// Executes both tools once and commits their evidence as one ingestion run.
    ///
    /// # Errors
    ///
    /// Returns an error when either MCP call fails or durable evidence cannot
    /// be validated and committed.
    pub async fn run_once(&self, run_id: &str) -> Result<(), DynError> {
        let free = self.ingest_yesterday_news(run_id).await?;
        let paid = self
            .ingest_realtime_news(run_id, self.fail_after_settlement_once)
            .await?;
        persist_run(&self.database, run_id, &free, &paid)?;
        println!("QuantStream Rust completed run {run_id}");
        Ok(())
    }

    /// Starts the two independently scheduled ingestion functions until
    /// process shutdown.
    ///
    /// # Errors
    ///
    /// Returns an error when scheduler creation, cron registration, startup,
    /// or shutdown fails.
    pub async fn run_scheduler(self: Arc<Self>) -> Result<(), DynError> {
        let mut scheduler = JobScheduler::new().await?;
        let shared = self;
        let delayed = Arc::clone(&shared);
        scheduler
            .add(Job::new_async(shared.yesterday_cron.as_str(), move |_id, _lock| {
                let job = Arc::clone(&delayed);
                Box::pin(async move {
                    let run_id = format!("rust-yesterday-{}", unix_time());
                    if let Err(error) = job.ingest_yesterday_news(&run_id).await {
                        eprintln!("delayed-news ingestion failed: {error}");
                    }
                })
            })?)
            .await?;
        let realtime = Arc::clone(&shared);
        scheduler
            .add(Job::new_async(shared.realtime_cron.as_str(), move |_id, _lock| {
                let job = Arc::clone(&realtime);
                Box::pin(async move {
                    let run_id = format!("rust-realtime-{}", unix_time());
                    if let Err(error) = job.ingest_realtime_news(&run_id, false).await {
                        eprintln!("realtime-news ingestion failed: {error}");
                    }
                })
            })?)
            .await?;
        scheduler.start().await?;
        wait_for_shutdown_signal().await;
        scheduler.shutdown().await?;
        Ok(())
    }
}

fn persist_run(path: &str, run_id: &str, free: &ToolResult, paid: &ToolResult) -> Result<(), DynError> {
    let mut connection = Connection::open(path)?;
    connection.execute_batch(
        "PRAGMA foreign_keys=ON;
         PRAGMA journal_mode=DELETE;
         PRAGMA synchronous=FULL;
         CREATE TABLE IF NOT EXISTS ingestion_run (
           run_id TEXT PRIMARY KEY,
           free_response_json TEXT NOT NULL,
           paid_response_json TEXT NOT NULL,
           payment_intent_id TEXT NOT NULL UNIQUE,
           completed_at INTEGER NOT NULL
         );",
    )?;
    let free_json: Value = serde_json::from_slice(&free.body)?;
    let paid_json: Value = serde_json::from_slice(&paid.body)?;
    let intent = paid
        .payment_intent_id
        .as_deref()
        .ok_or("paid result omitted payment intent")?;
    let transaction = connection.transaction()?;
    transaction.execute(
        "INSERT INTO ingestion_run(run_id,free_response_json,paid_response_json,payment_intent_id,completed_at)
         VALUES(?1,?2,?3,?4,?5)",
        params![
            run_id,
            serde_json::to_string(&free_json)?,
            serde_json::to_string(&paid_json)?,
            intent,
            unix_time()
        ],
    )?;
    transaction.commit()?;
    Ok(())
}

async fn wait_for_shutdown_signal() {
    #[cfg(unix)]
    if let Ok(mut terminate) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        tokio::select! { _ = tokio::signal::ctrl_c() => {}, _ = terminate.recv() => {} }
        return;
    }
    let _ = tokio::signal::ctrl_c().await;
}

fn required_env(name: &str) -> Result<String, DynError> {
    let value = env::var(name).map_err(|_| format!("{name} is required"))?;
    if value.trim().is_empty() {
        return Err(format!("{name} must not be blank").into());
    }
    Ok(value)
}

fn optional_env(name: &str, fallback: &str) -> String {
    env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_owned())
}

#[must_use]
pub fn unix_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
#[path = "../../tests/job/ingestion_job_test.rs"]
mod tests;
