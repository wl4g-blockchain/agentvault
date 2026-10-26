//! Generic `SQLite` repository: pool, prepared CRUD and transactions.

use crate::{Error, Result};
use serde::{de::DeserializeOwned, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteRow, SqliteSynchronous},
    Row as _, SqliteConnection, SqlitePool,
};
use std::{future::Future, path::Path, pin::Pin, str::FromStr as _};

#[cfg(test)]
#[path = "../../tests/storage/sqlite_test.rs"]
pub(crate) mod tests;

pub(crate) type ImmediateFuture<'connection, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'connection>>;

/// Cloneable `SQLite` implementation shared by composed domain repositories.
#[derive(Clone, Debug)]
pub struct SqliteRepository {
    pool: SqlitePool,
}

impl SqliteRepository {
    /// Constructs a lazy durable `SQLite` provider without opening a
    /// connection. [`crate::storage::IStorage::init`] owns connection and
    /// migration work.
    ///
    /// # Errors
    ///
    /// Fails closed for a non-absolute path, invalid connection settings,
    /// unsupported schema version, missing schema objects, or a disabled
    /// foreign-key pragma.
    pub fn new(database_file: &str, max_connections: u32, busy_timeout: std::time::Duration) -> Result<Self> {
        let path = Path::new(database_file);
        if !path.is_absolute() || max_connections == 0 || busy_timeout.is_zero() {
            return Err(Error::Config(
                "SQLite storage requires an absolute path, a positive pool size, and a positive busy timeout"
                    .to_owned(),
            ));
        }

        let options = SqliteConnectOptions::from_str("sqlite://")
            .map_err(|error| Error::Config(format!("invalid SQLite connection options: {error}")))?
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(SqliteSynchronous::Full)
            .busy_timeout(busy_timeout);

        let pool = SqlitePoolOptions::new()
            .max_connections(max_connections)
            .after_connect(|connection, _metadata| {
                Box::pin(async move {
                    sqlx::query("PRAGMA foreign_keys = ON")
                        .execute(&mut *connection)
                        .await?;
                    let enabled: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
                        .fetch_one(&mut *connection)
                        .await?;
                    if enabled != 1 {
                        return Err(sqlx::Error::Protocol(
                            "SQLite foreign-key enforcement is disabled".to_owned(),
                        ));
                    }
                    Ok(())
                })
            })
            .connect_lazy_with(options);

        Ok(Self { pool })
    }

    /// Returns a pool reference for adapter modules. It is intentionally not
    /// public API: callers should depend on the storage ports.
    #[must_use]
    pub const fn pool(&self) -> &SqlitePool {
        &self.pool
    }

    /// # Errors
    /// Returns schema validation, migration, or database errors.
    pub async fn migrate(&self, schema: &crate::storage::StorageSchema) -> Result<()> {
        schema.validate()?;
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| Self::storage_error("begin SQLite migration", error))?;
        let version: i64 = sqlx::query_scalar("PRAGMA user_version")
            .fetch_one(&mut *transaction)
            .await
            .map_err(|error| Self::storage_error("read SQLite schema version", error))?;
        if version == 0 {
            sqlx::raw_sql(schema.ddl)
                .execute(&mut *transaction)
                .await
                .map_err(|error| Self::storage_error("apply SQLite schema", error))?;
            sqlx::raw_sql(schema.dml)
                .execute(&mut *transaction)
                .await
                .map_err(|error| Self::storage_error("apply SQLite bootstrap", error))?;
        }
        let mut current = Self::schema_version(&mut transaction, schema).await?;
        if version != 0 && version != i64::from(current) {
            return Err(Error::Storage("SQLite version markers disagree".into()));
        }
        for step in schema.migrations {
            if step.to <= current {
                continue;
            }
            sqlx::raw_sql(step.sql)
                .execute(&mut *transaction)
                .await
                .map_err(|error| Self::storage_error("apply SQLite migration", error))?;
            current = Self::schema_version(&mut transaction, schema).await?;
            if current != step.to {
                return Err(Error::Storage("SQLite migration omitted its version marker".into()));
            }
        }
        schema.require_current(current)?;
        sqlx::query(&format!("PRAGMA user_version = {}", schema.version))
            .execute(&mut *transaction)
            .await
            .map_err(|error| Self::storage_error("record SQLite schema version", error))?;

        Self::verify_schema(&mut transaction, schema).await?;
        transaction
            .commit()
            .await
            .map_err(|error| Self::storage_error("commit SQLite migration", error))
    }

    /// Runs one authoritative write aggregate under `SQLite`'s eager writer
    /// lock. The closure must not commit or roll back the connection itself.
    /// # Errors
    /// Rolls back and returns the operation or database error.
    pub async fn transaction<T, Operation>(&self, operation: Operation) -> Result<T>
    where
        T: Send,
        Operation: for<'connection> FnOnce(&'connection mut SqliteConnection) -> ImmediateFuture<'connection, T> + Send,
    {
        self.immediate(operation).await
    }

    pub(crate) async fn immediate<T, Operation>(&self, operation: Operation) -> Result<T>
    where
        T: Send,
        Operation: for<'connection> FnOnce(&'connection mut SqliteConnection) -> ImmediateFuture<'connection, T> + Send,
    {
        let mut transaction = self
            .pool
            .begin_with("BEGIN IMMEDIATE")
            .await
            .map_err(|error| Self::storage_error("begin SQLite write transaction", error))?;
        match operation(&mut transaction).await {
            Ok(value) => {
                transaction
                    .commit()
                    .await
                    .map_err(|error| Self::storage_error("commit SQLite transaction", error))?;
                Ok(value)
            }
            Err(error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|error| Self::storage_error("roll back SQLite transaction", error))?;
                Err(error)
            }
        }
    }
}

impl crate::storage::IGenericStorage for SqliteRepository {
    type Database = sqlx::Sqlite;
    fn push_entity_value(
        query: &mut sqlx::QueryBuilder<'static, Self::Database>,
        value: crate::storage::EntityValue,
    ) -> Result<()> {
        use crate::storage::EntityValue;
        match value {
            EntityValue::Text(value) => {
                query.push_bind(value);
            }
            EntityValue::Bool(value) => {
                query.push_bind(value);
            }
            EntityValue::SmallInt(value) => {
                query.push_bind(value);
            }
            EntityValue::Integer(value) => {
                query.push_bind(value);
            }
            EntityValue::Bytes(value) => {
                query.push_bind(value);
            }
            EntityValue::Decimal(value) => {
                query.push_bind(value.map(|v| v.to_string()));
            }
            EntityValue::Json(value) => {
                query.push_bind(sqlx::types::Json(value));
            }
        }
        Ok(())
    }
}

impl SqliteRepository {
    async fn schema_version(connection: &mut SqliteConnection, schema: &crate::storage::StorageSchema) -> Result<i32> {
        let versions: Vec<i32> = sqlx::query_scalar(&format!(
            "SELECT version FROM {} ORDER BY version",
            schema.version_table
        ))
        .fetch_all(connection)
        .await
        .map_err(|error| Self::storage_error("read SQLite schema version", error))?;
        schema.current_version(&versions)
    }

    pub(crate) async fn verify_schema(
        connection: &mut SqliteConnection,
        schema: &crate::storage::StorageSchema,
    ) -> Result<()> {
        for table in schema.tables {
            let present: i64 =
                sqlx::query_scalar("SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = ?1")
                    .bind(table)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(|error| Self::storage_error("verify SQLite schema", error))?;
            if present != 1 {
                return Err(Error::Storage(format!(
                    "SQLite schema is missing required table {table}"
                )));
            }
        }
        schema.require_current(Self::schema_version(connection, schema).await?)?;
        Ok(())
    }

    pub(crate) fn storage_error(operation: &str, error: impl std::fmt::Display) -> Error {
        Error::Storage(format!("{operation}: {error}"))
    }

    pub(crate) fn encode_json<T: Serialize>(value: &T, field: &str) -> Result<String> {
        serde_json::to_string(value).map_err(|error| Error::Storage(format!("encode SQLite {field}: {error}")))
    }

    pub(crate) fn decode_json<T: DeserializeOwned>(value: impl AsRef<str>, field: &str) -> Result<T> {
        serde_json::from_str(value.as_ref()).map_err(|error| Error::Storage(format!("decode SQLite {field}: {error}")))
    }

    pub(crate) fn decode_u8(value: impl TryInto<u8>, field: &str) -> Result<u8> {
        value
            .try_into()
            .map_err(|_| Error::Storage(format!("SQLite {field} is out of range")))
    }

    pub(crate) fn decode_u32(value: i64, field: &str) -> Result<u32> {
        u32::try_from(value).map_err(|_| Error::Storage(format!("SQLite {field} is out of range")))
    }

    pub(crate) fn page_bounds(page: crate::storage::PageRequest) -> Result<(i64, i64)> {
        let offset =
            i64::try_from(page.offset).map_err(|_| Error::Storage("SQLite page offset is too large".to_owned()))?;
        Ok((i64::from(page.limit), offset))
    }

    pub(crate) fn field<T>(row: &SqliteRow, field_name: &str) -> Result<T>
    where
        for<'row> T: sqlx::Decode<'row, sqlx::Sqlite> + sqlx::Type<sqlx::Sqlite>,
    {
        row.try_get(field_name)
            .map_err(|error| Error::Storage(format!("decode SQLite column {field_name}: {error}")))
    }
}

/// Compatibility name for the dialect lifecycle provider. Domain operations are exposed by `StoragePorts`.
pub type SqliteStorage = SqliteRepository;

// Entity fields and values are resolved by the shared contract; SQLx owns binding.
macro_rules! dynamic_sqlite {
    (select, $entity:ty, $filters:expr) => {
        <crate::storage::sqlite::SqliteRepository as crate::storage::IGenericStorage>::select::<$entity>(
            &$filters, false,
        )
    };
    (count, $entity:ty, $filters:expr) => {
        <crate::storage::sqlite::SqliteRepository as crate::storage::IGenericStorage>::select::<$entity>(
            &$filters, true,
        )
    };
    (insert, $entity:expr) => {
        <crate::storage::sqlite::SqliteRepository as crate::storage::IGenericStorage>::insert($entity, false)
    };
    (insert_or_ignore, $entity:expr) => {
        <crate::storage::sqlite::SqliteRepository as crate::storage::IGenericStorage>::insert($entity, true)
    };
    (update, $entity:ty, $changes:expr, $filters:expr) => {
        <crate::storage::sqlite::SqliteRepository as crate::storage::IGenericStorage>::update::<$entity>(
            $changes, &$filters,
        )
    };
    (upsert, $entity:expr, $keys:expr, $updates:expr, $immutable:expr, $revision:expr) => {
        <crate::storage::sqlite::SqliteRepository as crate::storage::IGenericStorage>::upsert(
            $entity, $keys, $updates, $immutable, $revision,
        )
    };
    (delete_by, $entity:ty, $filters:expr) => {
        <crate::storage::sqlite::SqliteRepository as crate::storage::IGenericStorage>::delete_by::<$entity>(&$filters)
    };
}
pub(crate) use dynamic_sqlite;

crate::storage::entity_repository_queries!(SqliteRepository, SqliteRow, immediate, "");

/// Backward-compatible name for the generic repository.
pub type BaseSqliteRepository = SqliteRepository;
