// Generic entity CRUD and SQL-provider composition. Domain contracts live beside their repositories.

use crate::config::StorageConfig;
use crate::config::StorageProvider;
use crate::config::WalletKeyRingConfig;
use crate::storage::postgres::PostgresRepository;
use crate::storage::postgres::PostgresStorage;
use crate::storage::sqlite::SqliteRepository;
use crate::storage::sqlite::SqliteStorage;
use crate::storage::ApprovalStorage;
use crate::storage::ApproverStorage;
use crate::storage::ClientStorage;
use crate::storage::EntityQuery;
use crate::storage::EventStorage;
use crate::storage::LedgerStorage;
use crate::storage::OutboxStorage;
use crate::storage::PaymentStorage;
use crate::storage::PolicyStorage;
use crate::storage::PriceStorage;
use crate::storage::ScenarioStorage;
use crate::storage::SigningStorage;
use crate::storage::WalletBalanceStorage;
use crate::storage::WalletKeyManager;
use crate::storage::WalletPaymentSigner;
use crate::storage::WalletStorage;
use crate::utils::secret::SecretReader;
use crate::Result;
use std::{future::Future, pin::Pin, sync::Arc};

// Shared durable-storage contracts, provider lifecycle, and aggregate transaction ports.

const MAX_STORAGE_URL_BYTES: usize = 16 * 1024;

/// Provider-neutral handles for every durable payment domain port.
///
/// The composition root obtains these handles only from [`IStorage::ports`];
/// handlers never instantiate or downcast a SQL adapter.
#[derive(Clone)]
pub struct StoragePorts {
    pub clients: Arc<dyn ClientStorage>,
    pub scenarios: Arc<dyn ScenarioStorage>,
    pub approvers: Arc<dyn ApproverStorage>,
    pub policies: Arc<dyn PolicyStorage>,
    pub offers: Arc<dyn PriceStorage>,
    pub wallets: Arc<dyn WalletStorage>,
    pub wallet_balances: Arc<dyn WalletBalanceStorage>,
    pub payments: Arc<dyn PaymentStorage>,
    pub approvals: Arc<dyn ApprovalStorage>,
    pub events: Arc<dyn EventStorage>,
    pub ledger: Arc<dyn LedgerStorage>,
    pub outbox: Arc<dyn OutboxStorage>,
    pub signing: Arc<dyn SigningStorage>,
}

/// Unique lifecycle and domain contract implemented by every SQL provider.
///
/// Providers are constructed and consumed exclusively through
/// [`StorageFactory`]. The returned domain ports all share the same pool and
/// transaction authority.
pub trait IStorage: Send + Sync {
    fn provider(&self) -> StorageProvider;

    fn init(&self) -> StorageLifecycleFuture;

    /// Checks whether the authoritative SQL provider can serve a trivial
    /// query. Management readiness uses this signal to stop routing new
    /// payment work when durable authority is unavailable.
    fn ping(&self) -> StorageHealthFuture;

    fn close(&self) -> StorageCloseFuture;

    fn ports(self: Arc<Self>) -> StoragePorts;

    /// Builds the versioned local-wallet custody port for this SQL instance.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured wallet key ring cannot be loaded.
    fn wallet_key_manager(self: Arc<Self>, config: &WalletKeyRingConfig) -> Result<Arc<dyn WalletKeyManager>>;

    /// Builds the intent-bound payment signer for this SQL instance.
    ///
    /// # Errors
    ///
    /// Returns an error when the configured wallet key ring cannot be loaded.
    fn wallet_payment_signer(self: Arc<Self>, config: &WalletKeyRingConfig) -> Result<Arc<dyn WalletPaymentSigner>>;
}

/// Initialization performs `SQLx` migration batches whose executor futures are
/// intentionally awaited only by the single-threaded composition phase.
pub type StorageLifecycleFuture = Pin<Box<dyn Future<Output = Result<()>> + 'static>>;
pub type StorageHealthFuture = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;
pub type StorageCloseFuture = Pin<Box<dyn Future<Output = Result<()>> + Send + 'static>>;

/// Sole composition boundary for the authoritative storage provider.
#[derive(Default)]
pub struct StorageFactory;

impl StorageFactory {
    #[must_use]
    pub fn new() -> Self {
        Self
    }

    /// Selects and initializes the configured process-wide provider.
    ///
    /// # Errors
    ///
    /// Returns an error when configuration, secrets, or provider initialization
    /// fails.
    pub async fn create(&self, config: &StorageConfig) -> Result<Arc<dyn IStorage>> {
        let component: Arc<dyn IStorage> = match config.provider {
            StorageProvider::Sqlite => Arc::new(SqliteStorage::from_config(&config.sqlite)?),
            StorageProvider::Postgres => {
                let url =
                    SecretReader::read_ref(&config.postgres.url, "PostgreSQL payment URL", MAX_STORAGE_URL_BYTES)?;
                Arc::new(PostgresStorage::from_config(
                    &url,
                    config.postgres.max_connections,
                    config.postgres.connect_timeout,
                )?)
            }
        };
        if let Err(error) = component.init().await {
            let _ = component.close().await;
            return Err(error);
        }
        Ok(component)
    }
}

/// Bounded, offset-based page request used by management queries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PageRequest {
    pub offset: u64,
    pub limit: u32,
}

impl Default for PageRequest {
    fn default() -> Self {
        Self { offset: 0, limit: 10 }
    }
}

/// One page plus the total number of matching durable records.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    pub total: u64,
}

/// Outcome of an insert guarded by a durable domain replay identity.
///
/// Implementations must return `Replay` only when the stored request hash and
/// material fields match. Reusing the identity for different content is an error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IdempotentWrite<T> {
    Inserted(T),
    Replay(T),
}

#[cfg(test)]
#[path = "../../tests/storage/storage_test.rs"]
mod wallet_tests;

impl crate::storage::IStorage for PostgresStorage {
    fn provider(&self) -> crate::config::StorageProvider {
        crate::config::StorageProvider::Postgres
    }

    fn init(&self) -> crate::storage::StorageLifecycleFuture {
        let storage = self.clone();
        Box::pin(async move {
            storage
                .migrate(&crate::storage::schema::PaymentStorageSchema::postgres()?)
                .await
        })
    }

    fn ping(&self) -> crate::storage::StorageHealthFuture {
        let pool = self.pool().clone();
        Box::pin(async move {
            sqlx::query("SELECT 1")
                .execute(&pool)
                .await
                .map_err(|error| Self::storage_error("ping PostgreSQL payment storage", error))?;
            Ok(())
        })
    }

    fn close(&self) -> crate::storage::StorageCloseFuture {
        let pool = self.pool().clone();
        Box::pin(async move {
            pool.close().await;
            Ok(())
        })
    }

    fn ports(self: Arc<Self>) -> crate::storage::StoragePorts {
        crate::storage::StoragePorts {
            clients: Arc::new(crate::storage::client::postgres::ClientPostgresRepository::new(
                (*self).clone(),
            )),
            scenarios: Arc::new(crate::storage::scenario::postgres::ScenarioPostgresRepository::new(
                (*self).clone(),
            )),
            approvers: Arc::new(crate::storage::approver::postgres::ApproverPostgresRepository::new(
                (*self).clone(),
            )),
            policies: Arc::new(crate::storage::policy::postgres::PolicyPostgresRepository::new(
                (*self).clone(),
            )),
            offers: Arc::new(crate::storage::price::postgres::PricePostgresRepository::new(
                (*self).clone(),
            )),
            wallets: Arc::new(crate::storage::wallet::postgres::WalletPostgresRepository::new(
                (*self).clone(),
            )),
            wallet_balances: Arc::new(crate::storage::wallet::postgres::WalletPostgresRepository::new(
                (*self).clone(),
            )),
            payments: Arc::new(crate::storage::payment::postgres::PaymentPostgresRepository::new(
                (*self).clone(),
            )),
            approvals: Arc::new(crate::storage::approval::postgres::ApprovalPostgresRepository::new(
                (*self).clone(),
            )),
            events: Arc::new(crate::storage::event::postgres::EventPostgresRepository::new(
                (*self).clone(),
            )),
            ledger: Arc::new(crate::storage::ledger::postgres::LedgerPostgresRepository::new(
                (*self).clone(),
            )),
            outbox: Arc::new(crate::storage::outbox::postgres::OutboxPostgresRepository::new(
                (*self).clone(),
            )),
            signing: Arc::new(crate::storage::signing::postgres::SigningPostgresRepository::new(
                (*self).clone(),
            )),
        }
    }

    fn wallet_key_manager(
        self: Arc<Self>,
        config: &crate::config::WalletKeyRingConfig,
    ) -> Result<Arc<dyn crate::storage::WalletKeyManager>> {
        Ok(Arc::new(
            crate::storage::wallet::postgres::PgWalletKeyRepository::from_config((*self).clone(), config)?,
        ))
    }

    fn wallet_payment_signer(
        self: Arc<Self>,
        config: &crate::config::WalletKeyRingConfig,
    ) -> Result<Arc<dyn crate::storage::WalletPaymentSigner>> {
        Ok(Arc::new(
            crate::storage::wallet::postgres::PgWalletKeyRepository::from_config((*self).clone(), config)?,
        ))
    }
}

impl crate::storage::IStorage for SqliteStorage {
    fn provider(&self) -> crate::config::StorageProvider {
        crate::config::StorageProvider::Sqlite
    }

    fn init(&self) -> crate::storage::StorageLifecycleFuture {
        let storage = self.clone();
        Box::pin(async move {
            storage
                .migrate(&crate::storage::schema::PaymentStorageSchema::sqlite()?)
                .await
        })
    }

    fn ping(&self) -> crate::storage::StorageHealthFuture {
        let pool = self.pool().clone();
        Box::pin(async move {
            sqlx::query("SELECT 1")
                .execute(&pool)
                .await
                .map_err(|error| Self::storage_error("ping SQLite payment storage", error))?;
            Ok(())
        })
    }

    fn close(&self) -> crate::storage::StorageCloseFuture {
        let pool = self.pool().clone();
        Box::pin(async move {
            pool.close().await;
            Ok(())
        })
    }

    fn ports(self: Arc<Self>) -> crate::storage::StoragePorts {
        crate::storage::StoragePorts {
            clients: Arc::new(crate::storage::client::sqlite::ClientSqliteRepository::new(
                (*self).clone(),
            )),
            scenarios: Arc::new(crate::storage::scenario::sqlite::ScenarioSqliteRepository::new(
                (*self).clone(),
            )),
            approvers: Arc::new(crate::storage::approver::sqlite::ApproverSqliteRepository::new(
                (*self).clone(),
            )),
            policies: Arc::new(crate::storage::policy::sqlite::PolicySqliteRepository::new(
                (*self).clone(),
            )),
            offers: Arc::new(crate::storage::price::sqlite::PriceSqliteRepository::new(
                (*self).clone(),
            )),
            wallets: Arc::new(crate::storage::wallet::sqlite::WalletSqliteRepository::new(
                (*self).clone(),
            )),
            wallet_balances: Arc::new(crate::storage::wallet::sqlite::WalletSqliteRepository::new(
                (*self).clone(),
            )),
            payments: Arc::new(crate::storage::payment::sqlite::PaymentSqliteRepository::new(
                (*self).clone(),
            )),
            approvals: Arc::new(crate::storage::approval::sqlite::ApprovalSqliteRepository::new(
                (*self).clone(),
            )),
            events: Arc::new(crate::storage::event::sqlite::EventSqliteRepository::new(
                (*self).clone(),
            )),
            ledger: Arc::new(crate::storage::ledger::sqlite::LedgerSqliteRepository::new(
                (*self).clone(),
            )),
            outbox: Arc::new(crate::storage::outbox::sqlite::OutboxSqliteRepository::new(
                (*self).clone(),
            )),
            signing: Arc::new(crate::storage::signing::sqlite::SigningSqliteRepository::new(
                (*self).clone(),
            )),
        }
    }

    fn wallet_key_manager(
        self: Arc<Self>,
        config: &crate::config::WalletKeyRingConfig,
    ) -> Result<Arc<dyn crate::storage::WalletKeyManager>> {
        Ok(Arc::new(
            crate::storage::wallet::sqlite::SqliteWalletKeyRepository::from_config((*self).clone(), config)?,
        ))
    }

    fn wallet_payment_signer(
        self: Arc<Self>,
        config: &crate::config::WalletKeyRingConfig,
    ) -> Result<Arc<dyn crate::storage::WalletPaymentSigner>> {
        Ok(Arc::new(
            crate::storage::wallet::sqlite::SqliteWalletKeyRepository::from_config((*self).clone(), config)?,
        ))
    }
}

/// One ordered, transactional migration; SQL must append its target version marker.
#[derive(Clone, Copy)]
pub struct SchemaMigration {
    pub from: i32,
    pub to: i32,
    pub sql: &'static str,
}

/// Schema supplied by the composition root, independent of SQL driver code.
#[derive(Clone, Copy)]
pub struct StorageSchema {
    pub version: i32,
    pub version_table: &'static str,
    pub tables: &'static [&'static str],
    pub ddl: &'static str,
    pub dml: &'static str,
    pub advisory_lock: i64,
    pub migrations: &'static [SchemaMigration],
}
impl StorageSchema {
    pub(crate) fn validate(&self) -> Result<()> {
        if self.version <= 0
            || !EntityQuery::identifier(self.version_table)
            || self.tables.iter().any(|table| !EntityQuery::identifier(table))
        {
            return Err(crate::Error::Storage("invalid schema metadata".into()));
        }
        let mut current = self.migrations.first().map_or(self.version, |step| step.from);
        if current <= 0 {
            return Err(crate::Error::Storage("invalid initial schema version".into()));
        }
        for step in self.migrations {
            if step.from != current || step.to <= current || step.sql.trim().is_empty() {
                return Err(crate::Error::Storage("invalid schema migration sequence".into()));
            }
            current = step.to;
        }
        self.require_current(current)
    }

    pub(crate) fn current_version(&self, versions: &[i32]) -> Result<i32> {
        let initial = self.migrations.first().map_or(self.version, |step| step.from);
        let expected: Vec<_> = std::iter::once(initial)
            .chain(self.migrations.iter().map(|step| step.to))
            .collect();
        if versions.is_empty() || !expected.starts_with(versions) {
            return Err(crate::Error::Storage(format!(
                "unsupported schema versions {versions:?}"
            )));
        }
        Ok(*versions.last().expect("nonempty schema history"))
    }

    pub(crate) fn require_current(&self, current: i32) -> Result<()> {
        if current != self.version {
            return Err(crate::Error::Storage(format!(
                "schema version {current} does not reach {}",
                self.version
            )));
        }
        Ok(())
    }
}

impl SqliteStorage {
    pub(crate) fn from_config(config: &crate::config::SqliteStorageConfig) -> Result<Self> {
        Self::new(&config.database_file, config.max_connections, config.busy_timeout)
    }
}

impl PostgresRepository {
    #[cfg(test)]
    pub(crate) async fn connect(url: &str, max_connections: u32, connect_timeout: std::time::Duration) -> Result<Self> {
        use crate::storage::IStorage as _;

        let storage = Self::from_config(url, max_connections, connect_timeout)?;
        storage.init().await?;
        Ok(storage)
    }
}

impl PostgresRepository {
    pub(crate) fn from_config(url: &str, max_connections: u32, connect_timeout: std::time::Duration) -> Result<Self> {
        Self::new(url, max_connections, connect_timeout)
    }
}

impl SqliteRepository {
    #[cfg(test)]
    pub(crate) async fn connect(config: &crate::config::SqliteStorageConfig) -> Result<Self> {
        use crate::storage::IStorage as _;

        let storage = Self::from_config(config)?;
        storage.init().await?;
        Ok(storage)
    }
}
