//! Shared parameterized SQL and executable CRUD generated for both drivers.
use crate::{
    storage::{EntityValue, EntityValues, EntityWriteMode, Page, PageRequest, RepositoryEntity},
    Result,
};
use async_trait::async_trait;

/// Closed comparison operators; neither identifiers nor SQL fragments come from callers.
#[derive(Clone, Copy)]
pub enum EntityComparison {
    Equal,
    Less,
    LessOrEqual,
    Greater,
    GreaterOrEqual,
}
impl EntityComparison {
    const fn sql(self) -> &'static str {
        match self {
            Self::Equal => " = ",
            Self::Less => " < ",
            Self::LessOrEqual => " <= ",
            Self::Greater => " > ",
            Self::GreaterOrEqual => " >= ",
        }
    }
}

/// Controlled entity predicates, ordering and bound pagination.
#[derive(Clone, Default)]
pub struct EntityQuery {
    predicates: Vec<(&'static str, EntityComparison, EntityValue)>,
    order: Vec<(&'static str, bool)>,
    range: Option<(i64, i64)>,
}
impl EntityQuery {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    #[must_use]
    pub fn compare(mut self, field: &'static str, comparison: EntityComparison, value: impl Into<EntityValue>) -> Self {
        self.predicates.push((field, comparison, value.into()));
        self
    }
    #[must_use]
    pub fn eq(self, field: &'static str, value: impl Into<EntityValue>) -> Self {
        self.compare(field, EntityComparison::Equal, value)
    }
    #[must_use]
    pub fn order(mut self, field: &'static str, descending: bool) -> Self {
        self.order.push((field, descending));
        self
    }
    #[must_use]
    pub fn limit(mut self, limit: i64, offset: i64) -> Self {
        self.range = Some((limit, offset));
        self
    }
    fn validate<E: RepositoryEntity>(&self) -> Result<()> {
        for field in self
            .predicates
            .iter()
            .map(|p| p.0)
            .chain(self.order.iter().map(|p| p.0))
        {
            Self::validate_column::<E>(field)?;
        }
        if self.range.is_some_and(|(limit, offset)| limit <= 0 || offset < 0) {
            return Err(crate::Error::InvalidRequest("invalid repository pagination".into()));
        }
        Ok(())
    }
    pub(crate) fn validate_column<E: RepositoryEntity>(field: &str) -> Result<()> {
        if !Self::identifier(E::TABLE) || !Self::identifier(field) || !E::columns().contains(&field) {
            return Err(crate::Error::InvalidRequest("unknown repository entity column".into()));
        }
        Ok(())
    }
    pub(crate) fn identifier(field: &str) -> bool {
        !field.is_empty()
            && field
                .bytes()
                .enumerate()
                .all(|(index, byte)| byte.is_ascii_lowercase() || byte == b'_' || index > 0 && byte.is_ascii_digit())
    }
}

// SQLx performs entity decoding; domain repositories keep only their invariants.
macro_rules! entity_repository_queries {
    ($repository:ty, $row:ty, $transaction:ident, $share:literal) => {
        #[async_trait::async_trait]
        impl<E> crate::storage::IGenericRepository<E> for $repository
        where
            E: crate::storage::RepositoryEntity + Send + Sync + Unpin,
            for<'row> E: sqlx::FromRow<'row, $row>,
        {
            async fn select(
                &self,
                entity: &E,
                page: crate::storage::PageRequest,
            ) -> crate::Result<crate::storage::Page<E>> {
                let mut filters = crate::storage::EntityQuery::new();
                for (field, value) in entity.values()? {
                    filters = filters.eq(field, value);
                }
                self.entity_page(filters, page).await
            }
            async fn select_by(&self, key: crate::storage::EntityKey) -> crate::Result<Option<E>> {
                self.select_one(key.query::<E>()?).await
            }
            async fn insert(&self, entity: &E) -> crate::Result<E> {
                self.entity_insert_returning(entity).await
            }
            async fn update(&self, entity: &E) -> crate::Result<Option<E>> {
                let mut filters = crate::storage::EntityKey::of(entity)?.query::<E>()?;
                let mut changes = entity.values()?;
                for field in E::IMMUTABLE {
                    filters = filters.eq(
                        field,
                        changes
                            .get(field)
                            .cloned()
                            .ok_or_else(|| crate::Error::InvalidRequest("missing immutable entity column".into()))?,
                    );
                }
                if let Some(field) = E::REVISION {
                    filters = filters.compare(
                        field,
                        crate::storage::EntityComparison::Less,
                        changes
                            .get(field)
                            .cloned()
                            .ok_or_else(|| crate::Error::InvalidRequest("missing entity revision".into()))?,
                    );
                }
                changes.retain(|key, _| !E::KEYS.contains(key) && !E::IMMUTABLE.contains(key));
                self.entity_update_returning(changes, filters).await
            }
            async fn upsert(&self, entity: &E) -> crate::Result<Option<E>> {
                crate::storage::EntityKey::of(entity)?;
                let updates: Vec<_> = E::COLUMNS
                    .iter()
                    .copied()
                    .filter(|key| !E::KEYS.contains(key) && !E::IMMUTABLE.contains(key))
                    .collect();
                <Self as crate::storage::RepositorySql>::upsert(
                    entity,
                    E::KEYS,
                    &updates,
                    E::IMMUTABLE,
                    E::REVISION.unwrap_or(""),
                )?
                .push(" RETURNING *")
                .build_query_as::<E>()
                .fetch_optional(self.pool())
                .await
                .map_err(|error| Self::storage_error("upsert entity", error))
            }
            async fn delete_by(&self, key: crate::storage::EntityKey) -> crate::Result<u64> {
                <Self as crate::storage::RepositorySql>::delete_by::<E>(&key.query::<E>()?)?
                    .build()
                    .execute(self.pool())
                    .await
                    .map(|result| result.rows_affected())
                    .map_err(|error| Self::storage_error("delete entity", error))
            }
        }
        impl $repository {
            pub(crate) async fn select_one<E>(&self, filters: crate::storage::EntityQuery) -> crate::Result<Option<E>>
            where
                E: crate::storage::RepositoryEntity + Send + Unpin,
                for<'row> E: sqlx::FromRow<'row, $row>,
            {
                <Self as crate::storage::RepositorySql>::select::<E>(&filters, false)?
                    .build_query_as::<E>()
                    .fetch_optional(self.pool())
                    .await
                    .map_err(|error| Self::storage_error("select entity", error))
            }
            pub(crate) async fn select_by_connection<E>(
                connection: &mut <<Self as crate::storage::RepositorySql>::Database as sqlx::Database>::Connection,
                filters: crate::storage::EntityQuery,
            ) -> crate::Result<Option<E>>
            where
                E: crate::storage::RepositoryEntity + Send + Unpin,
                for<'row> E: sqlx::FromRow<'row, $row>,
            {
                <Self as crate::storage::RepositorySql>::select::<E>(&filters, false)?
                    .build_query_as::<E>()
                    .fetch_optional(connection)
                    .await
                    .map_err(|error| Self::storage_error("select entity in transaction", error))
            }
            pub(crate) async fn entity_unique<E>(
                &self,
                filters: crate::storage::EntityQuery,
            ) -> crate::Result<Option<E>>
            where
                E: crate::storage::RepositoryEntity + Send + Unpin,
                for<'row> E: sqlx::FromRow<'row, $row>,
            {
                let rows = <Self as crate::storage::RepositorySql>::select::<E>(&filters.limit(2, 0), false)?
                    .build_query_as::<E>()
                    .fetch_all(self.pool())
                    .await
                    .map_err(|error| Self::storage_error("resolve unique entity", error))?;
                if rows.len() > 1 {
                    return Err(crate::Error::Storage(
                        "multiple entities match a unique lookup".into(),
                    ));
                }
                Ok(rows.into_iter().next())
            }
            pub(crate) async fn entity_register<E>(
                &self,
                entity: &E,
            ) -> crate::Result<crate::storage::IdempotentWrite<E>>
            where
                E: crate::storage::RepositoryEntity + Send + Sync + Unpin + Clone + PartialEq + 'static,
                for<'row> E: sqlx::FromRow<'row, $row>,
            {
                let entity = entity.clone();
                self.$transaction(|connection| {
                    Box::pin(async move {
                        let identity = crate::storage::EntityKey::of(&entity)?.query::<E>()?;
                        let mut insert = <Self as crate::storage::RepositorySql>::insert(&entity, false)?;
                        insert
                            .push(" ON CONFLICT (")
                            .push(E::KEYS.join(", "))
                            .push(") DO NOTHING RETURNING *");
                        let inserted = insert
                            .build_query_as::<E>()
                            .fetch_optional(&mut *connection)
                            .await
                            .map_err(|error| {
                                crate::Error::StorageConflict(format!("entity registration rejected: {error}"))
                            })?;
                        if let Some(inserted) = inserted {
                            return Ok(crate::storage::IdempotentWrite::Inserted(inserted));
                        }
                        let existing = <Self as crate::storage::RepositorySql>::select::<E>(&identity, false)?
                            .push($share)
                            .build_query_as::<E>()
                            .fetch_one(&mut *connection)
                            .await
                            .map_err(|error| Self::storage_error("reload registered entity", error))?;
                        if existing != entity {
                            return Err(crate::Error::StorageConflict(
                                "entity registration identity is bound to different content".into(),
                            ));
                        }
                        Ok(crate::storage::IdempotentWrite::Replay(existing))
                    })
                })
                .await
            }

            pub(crate) async fn entity_page<E>(
                &self,
                mut filters: crate::storage::EntityQuery,
                page: crate::storage::PageRequest,
            ) -> crate::Result<crate::storage::Page<E>>
            where
                E: crate::storage::RepositoryEntity + Send + Unpin,
                for<'row> E: sqlx::FromRow<'row, $row>,
            {
                let (limit, offset) = Self::page_bounds(page)?;
                filters = filters.limit(limit, offset);
                let total: i64 = <Self as crate::storage::RepositorySql>::select::<E>(&filters, true)?
                    .build_query_scalar()
                    .fetch_one(self.pool())
                    .await
                    .map_err(|error| Self::storage_error("count entities", error))?;
                let items = <Self as crate::storage::RepositorySql>::select::<E>(&filters, false)?
                    .build_query_as::<E>()
                    .fetch_all(self.pool())
                    .await
                    .map_err(|error| Self::storage_error("page entities", error))?;
                Ok(crate::storage::Page {
                    items,
                    total: u64::try_from(total).map_err(|_| crate::Error::Storage("negative entity count".into()))?,
                })
            }
            pub(crate) async fn entity_insert_returning<E>(&self, entity: &E) -> crate::Result<E>
            where
                E: crate::storage::RepositoryEntity + Send + Unpin,
                for<'row> E: sqlx::FromRow<'row, $row>,
            {
                <Self as crate::storage::RepositorySql>::insert(entity, false)?
                    .push(" RETURNING *")
                    .build_query_as::<E>()
                    .fetch_one(self.pool())
                    .await
                    .map_err(|error| crate::Error::StorageConflict(format!("entity insert rejected: {error}")))
            }
            pub(crate) async fn entity_update_returning<E>(
                &self,
                changes: crate::storage::EntityValues,
                filters: crate::storage::EntityQuery,
            ) -> crate::Result<Option<E>>
            where
                E: crate::storage::RepositoryEntity + Send + Unpin,
                for<'row> E: sqlx::FromRow<'row, $row>,
            {
                <Self as crate::storage::RepositorySql>::update::<E>(changes, &filters)?
                    .push(" RETURNING *")
                    .build_query_as::<E>()
                    .fetch_optional(self.pool())
                    .await
                    .map_err(|error| Self::storage_error("update entity", error))
            }
        }
    };
}
pub(crate) use entity_repository_queries;

/// An entity-declared identity; positional components follow `RepositoryEntity::KEYS`.
#[derive(Clone)]
pub struct EntityKey(pub Vec<EntityValue>);
impl EntityKey {
    /// # Errors
    /// Rejects missing or NULL key components declared by the entity.
    pub fn of<E: RepositoryEntity>(entity: &E) -> Result<Self> {
        let mut values = entity.values()?;
        let keys = E::KEYS
            .iter()
            .map(|key| {
                values
                    .remove(key)
                    .ok_or_else(|| crate::Error::InvalidRequest("entity key is missing".into()))
            })
            .collect::<Result<Vec<_>>>()?;
        let key = Self(keys);
        key.query::<E>()?;
        Ok(key)
    }
    /// # Errors
    /// Rejects key arity, NULL keys, or unknown declared key columns.
    pub fn query<E: RepositoryEntity>(&self) -> Result<EntityQuery> {
        if E::KEYS.is_empty() || self.0.len() != E::KEYS.len() || self.0.iter().any(EntityValue::is_null) {
            return Err(crate::Error::InvalidRequest("invalid entity identity".into()));
        }
        let mut query = EntityQuery::new();
        for (name, value) in E::KEYS.iter().zip(&self.0) {
            query = query.eq(name, value.clone());
        }
        query.validate::<E>()?;
        Ok(query)
    }
}

/// Executable, entity-driven CRUD. Drivers implement this contract through the shared macro.
/// Predicates, identity, immutable fields and optional revision come from entity metadata.
#[async_trait]
pub trait IGenericRepository<E>: Send + Sync
where
    E: RepositoryEntity + Send + Sync,
{
    async fn select(&self, entity: &E, page: PageRequest) -> Result<Page<E>>;
    async fn select_by(&self, key: EntityKey) -> Result<Option<E>>;
    async fn insert(&self, entity: &E) -> Result<E>;
    async fn update(&self, entity: &E) -> Result<Option<E>>;
    async fn upsert(&self, entity: &E) -> Result<Option<E>>;
    async fn delete_by(&self, key: EntityKey) -> Result<u64>;
}

/// Shared prepared CRUD construction for SQL dialect repositories.
///
/// Compile-time entity metadata supplies controlled identifiers; `SQLx` binds values. Authoritative
/// financial writes remain explicit domain transactions on the same base pool.
pub trait RepositorySql {
    type Database: sqlx::Database;
    /// # Errors
    /// Returns an error if a typed value cannot be encoded by the driver.
    fn push_entity_value(query: &mut sqlx::QueryBuilder<'static, Self::Database>, value: EntityValue) -> Result<()>;

    /// # Errors
    /// Rejects unknown fields, invalid NULL comparisons, or unsupported ordered values.
    fn entity_predicates<E: RepositoryEntity>(
        query: &mut sqlx::QueryBuilder<'static, Self::Database>,
        filters: &EntityQuery,
    ) -> Result<()> {
        filters.validate::<E>()?;
        for (index, (field, comparison, value)) in filters.predicates.iter().enumerate() {
            query.push(if index == 0 { " WHERE " } else { " AND " }).push(*field);
            if value.is_null() {
                if !matches!(comparison, EntityComparison::Equal) {
                    return Err(crate::Error::InvalidRequest(
                        "NULL requires an equality predicate".into(),
                    ));
                }
                query.push(" IS NULL");
            } else {
                if matches!(value, EntityValue::Decimal(_)) && !matches!(comparison, EntityComparison::Equal) {
                    return Err(crate::Error::InvalidRequest(
                        "ordered decimal predicates require a compatible SQL encoding".into(),
                    ));
                }
                query.push(comparison.sql());
                Self::push_entity_value(query, value.clone())?;
            }
        }
        Ok(())
    }

    /// # Errors
    /// Rejects unknown fields, unsafe identifiers, or invalid pagination.
    fn select<E: RepositoryEntity>(
        filters: &EntityQuery,
        count: bool,
    ) -> Result<sqlx::QueryBuilder<'static, Self::Database>> {
        let mut query = sqlx::QueryBuilder::new(if count {
            "SELECT count(*) FROM "
        } else {
            "SELECT * FROM "
        });
        if !EntityQuery::identifier(E::TABLE) {
            return Err(crate::Error::Storage("invalid entity table".into()));
        }
        query.push(E::TABLE);
        Self::entity_predicates::<E>(&mut query, filters)?;
        if !count {
            for (index, (column, descending)) in filters.order.iter().enumerate() {
                query.push(if index == 0 { " ORDER BY " } else { ", " }).push(*column);
                if *descending {
                    query.push(" DESC");
                }
            }
            if let Some((limit, offset)) = filters.range {
                query.push(" LIMIT ");
                Self::push_entity_value(&mut query, limit.into())?;
                query.push(" OFFSET ");
                Self::push_entity_value(&mut query, offset.into())?;
            }
        }
        Ok(query)
    }

    /// # Errors
    /// Rejects a read-only entity, an empty record, or invalid column encoding.
    fn insert<E: RepositoryEntity>(
        entity: &E,
        ignore_conflict: bool,
    ) -> Result<sqlx::QueryBuilder<'static, Self::Database>> {
        if E::WRITE_MODE == EntityWriteMode::ReadOnly {
            return Err(crate::Error::InvalidRequest(
                "entity writes require its aggregate transaction".into(),
            ));
        }
        let fields = entity.values()?;
        if fields.is_empty() {
            return Err(crate::Error::InvalidRequest("empty repository entity".into()));
        }
        let mut query = sqlx::QueryBuilder::new("INSERT INTO ");
        query.push(E::TABLE).push(" (");
        for (index, field) in fields.keys().enumerate() {
            EntityQuery::validate_column::<E>(field)?;
            if index > 0 {
                query.push(", ");
            }
            query.push(field);
        }
        query.push(") VALUES (");
        for (index, value) in fields.into_values().enumerate() {
            if index > 0 {
                query.push(", ");
            }
            Self::push_entity_value(&mut query, value)?;
        }
        query.push(")");
        if ignore_conflict {
            query.push(" ON CONFLICT DO NOTHING");
        }
        Ok(query)
    }

    /// # Errors
    /// Rejects missing keys, identity mutations, or unsupported entity writes.
    fn upsert<E: RepositoryEntity>(
        entity: &E,
        keys: &[&'static str],
        updates: &[&'static str],
        immutable: &[&'static str],
        revision: &'static str,
    ) -> Result<sqlx::QueryBuilder<'static, Self::Database>> {
        if E::WRITE_MODE != EntityWriteMode::Catalog {
            return Err(crate::Error::InvalidRequest(
                "entity does not allow generic mutation".into(),
            ));
        }

        if keys.is_empty() || updates.is_empty() {
            return Err(crate::Error::InvalidRequest(
                "upsert requires identity and update columns".into(),
            ));
        }
        let mut query = Self::insert(entity, false)?;
        query.push(" ON CONFLICT (");
        for (index, field) in keys.iter().enumerate() {
            EntityQuery::validate_column::<E>(field)?;
            if index > 0 {
                query.push(", ");
            }
            query.push(*field);
        }
        query.push(") DO UPDATE SET ");
        for (index, field) in updates.iter().enumerate() {
            EntityQuery::validate_column::<E>(field)?;
            if keys.contains(field) || immutable.contains(field) {
                return Err(crate::Error::InvalidRequest("upsert cannot mutate identity".into()));
            }
            if index > 0 {
                query.push(", ");
            }
            query.push(*field).push(" = EXCLUDED.").push(*field);
        }
        if !immutable.is_empty() || !revision.is_empty() {
            query.push(" WHERE ");
        }
        for (index, field) in immutable.iter().enumerate() {
            EntityQuery::validate_column::<E>(field)?;
            if index > 0 {
                query.push(" AND ");
            }
            query
                .push("(")
                .push(E::TABLE)
                .push(".")
                .push(*field)
                .push(" = EXCLUDED.")
                .push(*field)
                .push(" OR (")
                .push(E::TABLE)
                .push(".")
                .push(*field)
                .push(" IS NULL AND EXCLUDED.")
                .push(*field)
                .push(" IS NULL))");
        }
        if !revision.is_empty() {
            if !immutable.is_empty() {
                query.push(" AND ");
            }
            EntityQuery::validate_column::<E>(revision)?;
            query
                .push("EXCLUDED.")
                .push(revision)
                .push(" > ")
                .push(E::TABLE)
                .push(".")
                .push(revision);
        }
        Ok(query)
    }

    /// # Errors
    /// Rejects empty or unfiltered mutations, immutable fields, or invalid encoding.
    fn update<E: RepositoryEntity>(
        changes: EntityValues,
        filters: &EntityQuery,
    ) -> Result<sqlx::QueryBuilder<'static, Self::Database>> {
        if E::WRITE_MODE != EntityWriteMode::Catalog {
            return Err(crate::Error::InvalidRequest(
                "entity does not allow generic mutation".into(),
            ));
        }

        if changes.is_empty() || filters.predicates.is_empty() {
            return Err(crate::Error::InvalidRequest(
                "entity update requires changes and predicates".into(),
            ));
        }
        let mut query = sqlx::QueryBuilder::new("UPDATE ");
        query.push(E::TABLE).push(" SET ");
        for (index, (field, value)) in changes.into_iter().enumerate() {
            EntityQuery::validate_column::<E>(field)?;
            if E::KEYS.contains(&field) || E::IMMUTABLE.contains(&field) {
                return Err(crate::Error::InvalidRequest("entity identity is immutable".into()));
            }
            if index > 0 {
                query.push(", ");
            }
            query.push(field).push(" = ");
            Self::push_entity_value(&mut query, value)?;
        }
        Self::entity_predicates::<E>(&mut query, filters)?;
        Ok(query)
    }

    /// # Errors
    /// Rejects unfiltered deletion or entities without generic mutation capability.
    fn delete_by<E: RepositoryEntity>(filters: &EntityQuery) -> Result<sqlx::QueryBuilder<'static, Self::Database>> {
        if E::WRITE_MODE != EntityWriteMode::Catalog {
            return Err(crate::Error::InvalidRequest(
                "entity does not allow generic mutation".into(),
            ));
        }

        if filters.predicates.is_empty() {
            return Err(crate::Error::InvalidRequest("entity delete requires predicates".into()));
        }
        let mut query = sqlx::QueryBuilder::new("DELETE FROM ");
        query.push(E::TABLE);
        Self::entity_predicates::<E>(&mut query, filters)?;
        Ok(query)
    }
}

#[cfg(test)]
#[path = "../../tests/storage/query_test.rs"]
mod tests;
