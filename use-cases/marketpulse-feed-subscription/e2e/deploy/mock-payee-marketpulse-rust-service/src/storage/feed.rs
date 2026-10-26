use std::sync::{Arc, Mutex};

use agentvault_sdk::{PaymentReceipt, SettledIncomingPayment};
use rusqlite::{params, Connection, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

const ACTORS: [(&str, &str); 10] = [
    ("Donald Trump", "United States president"),
    ("Marco Rubio", "United States secretary of state"),
    ("Ursula von der Leyen", "European Commission president"),
    ("Keir Starmer", "United Kingdom prime minister"),
    ("Emmanuel Macron", "French president"),
    ("Friedrich Merz", "German chancellor"),
    ("Shigeru Ishiba", "Japanese prime minister"),
    ("Narendra Modi", "Indian prime minister"),
    ("Luiz Inacio Lula da Silva", "Brazilian president"),
    ("Anthony Albanese", "Australian prime minister"),
];
const TOPICS: [(&str, &str, &str); 10] = [
    (
        "tariff",
        "proposed reciprocal tariff review for strategic imports",
        "SPY",
    ),
    ("chips", "outlined semiconductor export-control consultations", "SOXX"),
    ("energy", "commented on energy-security supply coordination", "XLE"),
    ("shipping", "addressed Red Sea shipping-risk countermeasures", "BDRY"),
    ("defense", "discussed allied defense procurement commitments", "ITA"),
    ("currency", "responded to cross-border currency volatility", "DXY"),
    ("sanctions", "announced a review of targeted trade sanctions", "EEM"),
    ("agriculture", "set out agricultural import quota negotiations", "DBA"),
    (
        "critical-minerals",
        "signed a critical-minerals supply memorandum",
        "LIT",
    ),
    ("digital-trade", "opened digital-services tax and AI trade talks", "QQQ"),
];

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NewsItem {
    pub id: String,
    pub observed_at: String,
    pub author: String,
    pub role: String,
    pub topic: String,
    pub symbols: Vec<String>,
    pub headline: String,
    pub source: String,
    pub sentiment_bps: i32,
}

#[derive(Clone)]
pub struct FeedSqliteRepository {
    connection: Arc<Mutex<Connection>>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredSettlement {
    pub intent_id: String,
    pub run_id: String,
    pub receipt: PaymentReceipt,
}

#[derive(Clone, Debug)]
pub enum DeliveryState {
    New,
    Settled(Box<StoredSettlement>),
    Delivered { response: String },
}

impl StoredSettlement {
    /// Projects a verified settlement into the durable delivery record.
    ///
    /// # Errors
    ///
    /// Returns an error when the settled intent belongs to a different run.
    pub fn from_payment(run_id: &str, payment: &SettledIncomingPayment) -> Result<Self, String> {
        if payment.intent.run_id != run_id {
            return Err("settlement is not bound to the workflow run".to_owned());
        }
        Ok(Self {
            intent_id: payment.intent.id.clone(),
            run_id: run_id.to_owned(),
            receipt: payment.receipt.clone(),
        })
    }
}

impl FeedSqliteRepository {
    /// Opens the durable feed store and initializes its fixed dataset.
    ///
    /// # Errors
    ///
    /// Returns an error for a non-persistent path, a `SQLite` failure, or an
    /// existing partial seed that does not contain exactly 100 rows.
    pub fn open(path: &str) -> Result<Self, String> {
        if path.trim().is_empty() || path == ":memory:" {
            return Err("MARKETPULSE_DATABASE must be a persistent SQLite database".to_owned());
        }
        let mut connection = Connection::open(path).map_err(|error| format!("open MarketPulse database: {error}"))?;
        connection
            .execute_batch(
                "PRAGMA foreign_keys=ON;
                 PRAGMA journal_mode=DELETE;
                 PRAGMA synchronous=FULL;
                 CREATE TABLE IF NOT EXISTS feed_item(
                   id TEXT PRIMARY KEY,observed_at TEXT NOT NULL,author TEXT NOT NULL,role TEXT NOT NULL,
                   topic TEXT NOT NULL,symbols_json TEXT NOT NULL,headline TEXT NOT NULL,source TEXT NOT NULL,
                   sentiment_bps INTEGER NOT NULL,access_tier TEXT NOT NULL CHECK(access_tier IN ('yesterday','realtime'))
                 );
                 CREATE TABLE IF NOT EXISTS mcp_delivery(
                   run_id TEXT PRIMARY KEY,request_hash TEXT NOT NULL,intent_id TEXT,settlement_json TEXT NOT NULL,
                   response_json TEXT,status TEXT NOT NULL CHECK(status IN ('settled','delivered')),
                   failure_injections INTEGER NOT NULL DEFAULT 0 CHECK(failure_injections IN (0,1)),updated_at INTEGER NOT NULL
                 );",
            )
            .map_err(|error| format!("initialize MarketPulse database: {error}"))?;
        seed(&mut connection)?;
        Ok(Self {
            connection: Arc::new(Mutex::new(connection)),
        })
    }

    /// Reads the free previous-day feed, capped by `limit`.
    ///
    /// # Errors
    ///
    /// Returns an error when the `SQLite` lock or query fails.
    pub fn yesterday_news(&self, limit: usize) -> Result<Vec<NewsItem>, String> {
        self.news("yesterday", limit)
    }

    /// Reads the paid real-time feed, capped by `limit`.
    ///
    /// # Errors
    ///
    /// Returns an error when the `SQLite` lock or query fails.
    pub fn realtime_news(&self, limit: usize) -> Result<Vec<NewsItem>, String> {
        self.news("realtime", limit)
    }

    fn news(&self, tier: &str, limit: usize) -> Result<Vec<NewsItem>, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "MarketPulse database lock is poisoned".to_owned())?;
        let mut statement = connection
            .prepare(
                "SELECT id,observed_at,author,role,topic,symbols_json,headline,source,sentiment_bps
                 FROM feed_item WHERE access_tier=?1 ORDER BY id LIMIT ?2",
            )
            .map_err(|error| format!("prepare feed query: {error}"))?;
        let rows = statement
            .query_map(params![tier, i64::try_from(limit).unwrap_or(i64::MAX)], |row| {
                let symbols: String = row.get(5)?;
                Ok(NewsItem {
                    id: row.get(0)?,
                    observed_at: row.get(1)?,
                    author: row.get(2)?,
                    role: row.get(3)?,
                    topic: row.get(4)?,
                    symbols: serde_json::from_str(&symbols).unwrap_or_default(),
                    headline: row.get(6)?,
                    source: row.get(7)?,
                    sentiment_bps: row.get(8)?,
                })
            })
            .map_err(|error| format!("query feed: {error}"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|error| format!("decode feed: {error}"))
    }

    /// Loads a delivery while enforcing the run/request-hash binding.
    ///
    /// # Errors
    ///
    /// Returns an error for a `SQLite` failure, a conflicting request hash, or
    /// malformed durable settlement state.
    pub fn delivery(&self, run_id: &str, request_hash: &str) -> Result<DeliveryState, String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "MarketPulse database lock is poisoned".to_owned())?;
        let row = connection
            .query_row(
                "SELECT request_hash,settlement_json,response_json,status FROM mcp_delivery WHERE run_id=?1",
                [run_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()
            .map_err(|error| format!("read MCP delivery: {error}"))?;
        let Some((stored_hash, settlement, response, status)) = row else {
            return Ok(DeliveryState::New);
        };
        if stored_hash != request_hash {
            return Err("MCP run_id was reused for different request content".to_owned());
        }
        let settlement =
            serde_json::from_str(&settlement).map_err(|error| format!("decode durable settlement: {error}"))?;
        match (status.as_str(), response) {
            ("settled", None) => Ok(DeliveryState::Settled(Box::new(settlement))),
            ("delivered", Some(response)) => Ok(DeliveryState::Delivered { response }),
            _ => Err("invalid durable MCP delivery state".to_owned()),
        }
    }

    /// Persists the first confirmed settlement for a run.
    ///
    /// # Errors
    ///
    /// Returns an error for a `SQLite` failure or if the run ID is already
    /// bound to different request or settlement evidence.
    pub fn record_settled(
        &self,
        run_id: &str,
        request_hash: &str,
        settlement: &StoredSettlement,
    ) -> Result<(), String> {
        let mut connection = self
            .connection
            .lock()
            .map_err(|_| "MarketPulse database lock is poisoned".to_owned())?;
        let encoded = serde_json::to_string(settlement).map_err(|error| error.to_string())?;
        let transaction = connection
            .transaction()
            .map_err(|error| format!("begin delivery transaction: {error}"))?;
        let inserted = transaction
            .execute(
                "INSERT INTO mcp_delivery(run_id,request_hash,intent_id,settlement_json,response_json,status,updated_at)
                 VALUES(?1,?2,?3,?4,NULL,'settled',?5) ON CONFLICT(run_id) DO NOTHING",
                params![run_id, request_hash, settlement.intent_id, encoded, unix_time()],
            )
            .map_err(|error| format!("record settled delivery: {error}"))?;
        if inserted == 0 {
            let existing = transaction
                .query_row(
                    "SELECT request_hash,intent_id,settlement_json FROM mcp_delivery WHERE run_id=?1",
                    [run_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, String>(2)?,
                        ))
                    },
                )
                .map_err(|error| format!("read settled replay: {error}"))?;
            if existing != (request_hash.to_owned(), settlement.intent_id.clone(), encoded) {
                return Err("MCP run_id is already bound to another settlement".to_owned());
            }
        }
        transaction
            .commit()
            .map_err(|error| format!("commit delivery settlement: {error}"))
    }

    /// Atomically marks a settled delivery as delivered.
    ///
    /// # Errors
    ///
    /// Returns an error for a `SQLite` failure or when no matching settled row exists.
    pub fn record_delivered(&self, run_id: &str, request_hash: &str, response: &str) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "MarketPulse database lock is poisoned".to_owned())?;
        let changed = connection
            .execute(
                "UPDATE mcp_delivery SET response_json=?1,status='delivered',updated_at=?2
                 WHERE run_id=?3 AND request_hash=?4 AND status='settled'",
                params![response, unix_time(), run_id, request_hash],
            )
            .map_err(|error| format!("complete MCP delivery: {error}"))?;
        if changed != 1 {
            return Err("MCP delivery was not in settled state".to_owned());
        }
        Ok(())
    }

    /// Consumes the one-shot post-settlement failure injection used by the E2E.
    ///
    /// # Errors
    ///
    /// Returns an error for a `SQLite` failure or if the injection was already consumed.
    pub fn record_failure_injection(&self, run_id: &str, request_hash: &str) -> Result<(), String> {
        let connection = self
            .connection
            .lock()
            .map_err(|_| "MarketPulse database lock is poisoned".to_owned())?;
        let changed = connection
            .execute(
                "UPDATE mcp_delivery SET failure_injections=1,updated_at=?1
                 WHERE run_id=?2 AND request_hash=?3 AND status='settled' AND failure_injections=0",
                params![unix_time(), run_id, request_hash],
            )
            .map_err(|error| format!("record failure injection: {error}"))?;
        if changed != 1 {
            return Err("post-settlement failure injection was already consumed".to_owned());
        }
        Ok(())
    }

    #[must_use]
    pub fn request_hash(body: &[u8]) -> String {
        let mut hasher = Sha256::new();
        hasher.update(b"marketpulse.mcp.request.v1\0");
        hasher.update(body);
        format!("{hash:x}", hash = hasher.finalize())
    }
}

fn seed(connection: &mut Connection) -> Result<(), String> {
    let count: i64 = connection
        .query_row("SELECT COUNT(*) FROM feed_item", [], |row| row.get(0))
        .map_err(|error| format!("count feed seed: {error}"))?;
    if count == 100 {
        return Ok(());
    }
    if count != 0 {
        return Err(format!(
            "MarketPulse feed seed must contain exactly 100 rows, found {count}"
        ));
    }
    let transaction = connection
        .transaction()
        .map_err(|error| format!("begin feed seed: {error}"))?;
    for index in 0..100 {
        let (actor, role) = ACTORS[index % ACTORS.len()];
        let (topic, event, symbol) = TOPICS[(index / ACTORS.len()) % TOPICS.len()];
        let tier = if index < 50 { "yesterday" } else { "realtime" };
        let observed = if index < 50 {
            format!("2026-10-14T{:02}:{:02}:00Z", index % 24, index % 60)
        } else {
            format!("2026-10-15T{:02}:{:02}:30Z", index % 24, index % 60)
        };
        transaction
            .execute(
                "INSERT INTO feed_item VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    format!("marketpulse-{index:03}"),
                    observed,
                    actor,
                    role,
                    topic,
                    serde_json::to_string(&vec![symbol]).map_err(|error| error.to_string())?,
                    format!("{actor} {event}; markets assess geopolitical and tariff implications"),
                    if index < 50 {
                        "MarketPulse public archive"
                    } else {
                        "MarketPulse live wire"
                    },
                    (i32::try_from(index).unwrap_or_default() - 50) * 7,
                    tier,
                ],
            )
            .map_err(|error| format!("insert feed seed: {error}"))?;
    }
    transaction
        .commit()
        .map_err(|error| format!("commit feed seed: {error}"))
}

fn unix_time() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
#[path = "../../tests/storage/feed_test.rs"]
mod tests;
