//! Generic `PostgreSQL` repository: pool, prepared CRUD and transactions.

use crate::{Error, Result};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use sqlx::{
    postgres::{PgConnectOptions, PgPoolOptions, PgRow},
    PgConnection, PgPool, Row as _,
};
use std::{fmt, future::Future, pin::Pin, str::FromStr as _, time::Duration};

#[cfg(test)]
#[path = "../../tests/storage/postgres_test.rs"]
pub(crate) mod tests;

pub(crate) type TransactionFuture<'connection, T> = Pin<Box<dyn Future<Output = Result<T>> + Send + 'connection>>;

/// Cloneable `PostgreSQL` implementation shared by all repositories.
#[derive(Clone)]
pub struct PostgresRepository {
    pool: PgPool,
    connect_timeout: Duration,
}

impl PostgresRepository {
    /// Constructs a lazy database pool. The factory invokes
    /// [`crate::storage::IStorage::init`] after registration to establish the
    /// connection and execute the supplied schema plan.
    ///
    /// The URL is accepted as already-resolved sensitive material and is never
    /// retained by this adapter or included in an error.
    ///
    /// # Errors
    ///
    /// Fails for invalid limits, an invalid/unreachable database, a partial or
    /// unsupported schema, or a migration/advisory-lock failure.
    pub fn new(url: &str, max_connections: u32, connect_timeout: Duration) -> Result<Self> {
        if url.trim().is_empty() || max_connections == 0 || connect_timeout.is_zero() {
            return Err(Error::Config(
                "PostgreSQL storage requires a URL and positive connection limits".to_owned(),
            ));
        }
        if url.starts_with("jdbc:") {
            return Err(Error::Config(
                "PostgreSQL storage URL must use postgres:// or postgresql://; JDBC URLs require a JVM driver"
                    .to_owned(),
            ));
        }
        let options = PgConnectOptions::from_str(url)
            .map_err(|_| Error::Config("PostgreSQL storage URL is invalid".to_owned()))?;
        let pool = PgPoolOptions::new()
            .max_connections(max_connections)
            .acquire_timeout(connect_timeout)
            .after_connect(|connection, _metadata| {
                Box::pin(async move {
                    sqlx::query("SET TIME ZONE 'UTC'").execute(connection).await?;
                    Ok(())
                })
            })
            .connect_lazy_with(options);
        Ok(Self { pool, connect_timeout })
    }

    #[must_use]
    pub const fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Runs one authoritative aggregate operation in a database transaction.
    ///
    /// # Errors
    ///
    /// Propagates the operation error, or fails closed if begin, commit, or
    /// rollback cannot be completed.
    pub async fn transaction<T, Operation>(&self, operation: Operation) -> Result<T>
    where
        T: Send,
        Operation: for<'connection> FnOnce(&'connection mut PgConnection) -> TransactionFuture<'connection, T> + Send,
    {
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| Self::storage_error("begin PostgreSQL transaction", error))?;
        match operation(&mut transaction).await {
            Ok(value) => {
                transaction
                    .commit()
                    .await
                    .map_err(|error| Self::storage_error("commit PostgreSQL transaction", error))?;
                Ok(value)
            }
            Err(operation_error) => {
                transaction
                    .rollback()
                    .await
                    .map_err(|error| Self::storage_error("roll back PostgreSQL transaction", error))?;
                Err(operation_error)
            }
        }
    }

    /// # Errors
    /// Returns schema validation, migration, or database errors.
    pub async fn migrate(&self, schema: &crate::storage::StorageSchema) -> Result<()> {
        schema.validate()?;
        let mut transaction = self
            .pool
            .begin()
            .await
            .map_err(|error| Self::storage_error("begin migration", error))?;
        tokio::time::timeout(
            self.connect_timeout,
            sqlx::query("SELECT pg_advisory_xact_lock($1)")
                .bind(schema.advisory_lock)
                .execute(&mut *transaction),
        )
        .await
        .map_err(|_| Error::Storage("PostgreSQL migration lock timed out".into()))?
        .map_err(|error| Self::storage_error("lock migration", error))?;
        Self::migrate_locked(&mut transaction, schema).await?;
        transaction
            .commit()
            .await
            .map_err(|error| Self::storage_error("commit migration", error))
    }
}

impl crate::storage::IGenericStorage for PostgresRepository {
    type Database = sqlx::Postgres;
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
                query.push_bind(value);
            }
            EntityValue::Json(value) => {
                query.push_bind(sqlx::types::Json(value));
            }
        }
        Ok(())
    }
}

impl fmt::Debug for PostgresRepository {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PostgresStorage")
            .field("pool", &"<redacted>")
            .field("connect_timeout", &self.connect_timeout)
            .finish()
    }
}

impl PostgresRepository {
    pub(crate) async fn migrate_locked(
        connection: &mut PgConnection,
        schema: &crate::storage::StorageSchema,
    ) -> Result<()> {
        let marker: Option<String> =
            sqlx::query_scalar("SELECT to_regclass(format('%I.%I', current_schema(), $1::text))::text")
                .bind(schema.version_table)
                .fetch_one(&mut *connection)
                .await
                .map_err(|error| Self::storage_error("inspect PostgreSQL schema", error))?;
        if marker.is_none() {
            let existing: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = current_schema() AND c.relname = ANY($1)",
            )
            .bind(schema.tables)
            .fetch_one(&mut *connection)
            .await
            .map_err(|error| Self::storage_error("detect partial PostgreSQL schema", error))?;
            if existing != 0 {
                return Err(Error::Storage(
                    "PostgreSQL schema is partial or has no version marker".to_owned(),
                ));
            }
            sqlx::raw_sql(schema.ddl)
                .execute(&mut *connection)
                .await
                .map_err(|error| Self::storage_error("apply PostgreSQL schema", error))?;
            sqlx::raw_sql(schema.dml)
                .execute(&mut *connection)
                .await
                .map_err(|error| Self::storage_error("apply PostgreSQL bootstrap data", error))?;
        }

        let mut current = Self::schema_version(connection, schema).await?;
        for step in schema.migrations {
            if step.to <= current {
                continue;
            }
            sqlx::raw_sql(step.sql)
                .execute(&mut *connection)
                .await
                .map_err(|error| Self::storage_error("apply PostgreSQL migration", error))?;
            current = Self::schema_version(connection, schema).await?;
            if current != step.to {
                return Err(Error::Storage("PostgreSQL migration omitted its version marker".into()));
            }
        }
        schema.require_current(current)?;
        Self::verify_schema(connection, schema).await
    }

    async fn schema_version(connection: &mut PgConnection, schema: &crate::storage::StorageSchema) -> Result<i32> {
        let versions: Vec<i32> = sqlx::query_scalar(&format!(
            "SELECT version FROM {} ORDER BY version",
            schema.version_table
        ))
        .fetch_all(connection)
        .await
        .map_err(|error| Self::storage_error("read PostgreSQL schema version", error))?;
        schema.current_version(&versions)
    }

    pub(crate) async fn verify_schema(
        connection: &mut PgConnection,
        schema: &crate::storage::StorageSchema,
    ) -> Result<()> {
        for table in schema.tables {
            let present: Option<String> =
                sqlx::query_scalar("SELECT to_regclass(format('%I.%I', current_schema(), $1::text))::text")
                    .bind(*table)
                    .fetch_one(&mut *connection)
                    .await
                    .map_err(|error| Self::storage_error("verify PostgreSQL schema", error))?;
            if present.is_none() {
                return Err(Error::Storage(format!(
                    "PostgreSQL schema is missing required table {table}"
                )));
            }
        }
        Ok(())
    }

    pub(crate) fn storage_error(operation: &str, error: impl fmt::Display) -> Error {
        Error::Storage(format!("{operation}: {error}"))
    }

    pub(crate) fn encode_json<T: Serialize>(value: &T, field: &str) -> Result<Value> {
        serde_json::to_value(value).map_err(|error| Error::Storage(format!("encode PostgreSQL {field}: {error}")))
    }

    pub(crate) fn decode_json<T: DeserializeOwned>(value: Value, field: &str) -> Result<T> {
        serde_json::from_value(value).map_err(|error| Error::Storage(format!("decode PostgreSQL {field}: {error}")))
    }

    pub(crate) fn decode_u8(value: i16, field: &str) -> Result<u8> {
        u8::try_from(value).map_err(|_| Error::Storage(format!("PostgreSQL {field} is out of range")))
    }

    pub(crate) fn decode_u32(value: i64, field: &str) -> Result<u32> {
        u32::try_from(value).map_err(|_| Error::Storage(format!("PostgreSQL {field} is out of range")))
    }

    pub(crate) fn page_bounds(page: crate::storage::PageRequest) -> Result<(i64, i64)> {
        let offset =
            i64::try_from(page.offset).map_err(|_| Error::Storage("PostgreSQL page offset is too large".to_owned()))?;
        Ok((i64::from(page.limit), offset))
    }

    pub(crate) fn field<T>(row: &PgRow, field_name: &str) -> Result<T>
    where
        for<'row> T: sqlx::Decode<'row, sqlx::Postgres> + sqlx::Type<sqlx::Postgres>,
    {
        row.try_get(field_name)
            .map_err(|error| Error::Storage(format!("decode PostgreSQL column {field_name}: {error}")))
    }
}

/// Compatibility name for the dialect lifecycle provider. Domain operations are exposed by `StoragePorts`.
pub type PostgresStorage = PostgresRepository;

// Entity fields and values are resolved by the shared contract; SQLx owns binding.
macro_rules! dynamic_postgres {
    (select, $entity:ty, $filters:expr) => {
        <crate::storage::postgres::PostgresRepository as crate::storage::IGenericStorage>::select::<$entity>(
            &$filters, false,
        )
    };
    (count, $entity:ty, $filters:expr) => {
        <crate::storage::postgres::PostgresRepository as crate::storage::IGenericStorage>::select::<$entity>(
            &$filters, true,
        )
    };
    (insert, $entity:expr) => {
        <crate::storage::postgres::PostgresRepository as crate::storage::IGenericStorage>::insert($entity, false)
    };
    (insert_or_ignore, $entity:expr) => {
        <crate::storage::postgres::PostgresRepository as crate::storage::IGenericStorage>::insert($entity, true)
    };
    (update, $entity:ty, $changes:expr, $filters:expr) => {
        <crate::storage::postgres::PostgresRepository as crate::storage::IGenericStorage>::update::<$entity>(
            $changes, &$filters,
        )
    };
    (upsert, $entity:expr, $keys:expr, $updates:expr, $immutable:expr, $revision:expr) => {
        <crate::storage::postgres::PostgresRepository as crate::storage::IGenericStorage>::upsert(
            $entity, $keys, $updates, $immutable, $revision,
        )
    };
    (delete_by, $entity:ty, $filters:expr) => {
        <crate::storage::postgres::PostgresRepository as crate::storage::IGenericStorage>::delete_by::<$entity>(
            &$filters,
        )
    };
}
pub(crate) use dynamic_postgres;

crate::storage::entity_repository_queries!(PostgresRepository, PgRow, transaction, " FOR SHARE");

/// Backward-compatible name for the generic repository.
pub type BasePostgresRepository = PostgresRepository;
