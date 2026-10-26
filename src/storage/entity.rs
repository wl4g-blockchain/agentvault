//! Compile-time entity metadata and typed SQL values.
use crate::Result;
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum EntityWriteMode {
    ReadOnly,
    AppendOnly,
    Catalog,
}

/// Generic owned SQL arguments. JSON is used only for actual JSON columns.
#[derive(Clone, Debug)]
pub enum EntityValue {
    Text(Option<String>),
    Bool(Option<bool>),
    SmallInt(Option<i16>),
    Integer(Option<i64>),
    Decimal(Option<bigdecimal::BigDecimal>),
    Bytes(Option<Vec<u8>>),
    Json(Value),
}
impl EntityValue {
    pub(crate) fn is_null(&self) -> bool {
        match self {
            Self::Text(v) => v.is_none(),
            Self::Bool(v) => v.is_none(),
            Self::SmallInt(v) => v.is_none(),
            Self::Integer(v) => v.is_none(),
            Self::Decimal(v) => v.is_none(),
            Self::Bytes(v) => v.is_none(),
            Self::Json(_) => false,
        }
    }
    /// Encodes a declared JSON column, never an entity or SQL predicate.
    /// # Errors
    /// Returns an error if the JSON column cannot be serialized.
    pub fn json(value: &impl Serialize) -> Result<Self> {
        serde_json::to_value(value)
            .map(Self::Json)
            .map_err(|_| crate::Error::Storage("invalid JSON column".into()))
    }
}
macro_rules! entity_scalar_values {
    ($($ty:ty => $variant:ident),* $(,)?) => {$ (
        impl From<$ty> for EntityValue { fn from(value: $ty) -> Self { Self::$variant(Some(value)) } }
        impl From<&$ty> for EntityValue { fn from(value: &$ty) -> Self { Self::from(value.clone()) } }
        impl From<Option<$ty>> for EntityValue { fn from(value: Option<$ty>) -> Self { Self::$variant(value) } }
        impl From<&Option<$ty>> for EntityValue { fn from(value: &Option<$ty>) -> Self { Self::$variant(value.clone()) } }
    )*};
}
entity_scalar_values! { String => Text, bool => Bool, i16 => SmallInt, i64 => Integer,
bigdecimal::BigDecimal => Decimal, Vec<u8> => Bytes }
impl From<&str> for EntityValue {
    fn from(value: &str) -> Self {
        Self::Text(Some(value.into()))
    }
}
impl From<&u8> for EntityValue {
    fn from(value: &u8) -> Self {
        Self::from(i16::from(*value))
    }
}
impl From<&u32> for EntityValue {
    fn from(value: &u32) -> Self {
        Self::from(i64::from(*value))
    }
}

/// Field layout emitted from the entity declaration at compile time.
pub enum EntityField {
    Column(&'static str),
    Flatten(&'static [Self]),
}
impl EntityField {
    #[must_use]
    pub const fn count(fields: &[Self]) -> usize {
        let mut count = 0;
        let mut i = 0;
        while i < fields.len() {
            count += match fields[i] {
                Self::Column(_) => 1,
                Self::Flatten(nested) => Self::count(nested),
            };
            i += 1;
        }
        count
    }
    #[must_use]
    pub const fn columns<const N: usize>(fields: &[Self]) -> [&'static str; N] {
        let mut columns = [""; N];
        Self::collect(fields, &mut columns, 0);
        columns
    }
    const fn collect(fields: &[Self], result: &mut [&'static str], mut offset: usize) -> usize {
        let mut i = 0;
        while i < fields.len() {
            match fields[i] {
                Self::Column(name) => {
                    result[offset] = name;
                    offset += 1;
                }
                Self::Flatten(nested) => {
                    offset = Self::collect(nested, result, offset);
                }
            }
            i += 1;
        }
        offset
    }
}
pub type EntityValues = std::collections::BTreeMap<&'static str, EntityValue>;

/// Metadata generated from the canonical entity; no runtime reflection.
pub trait RepositoryEntity {
    const TABLE: &'static str;
    const FIELDS: &'static [EntityField];
    const COLUMNS: &'static [&'static str];
    const WRITE_MODE: EntityWriteMode = EntityWriteMode::ReadOnly;
    const KEYS: &'static [&'static str] = &[];
    const IMMUTABLE: &'static [&'static str] = &[];
    const REVISION: Option<&'static str> = None;
    #[must_use]
    fn columns() -> &'static [&'static str] {
        Self::COLUMNS
    }
    /// Returns typed SQL values generated from the entity's fields.
    /// # Errors
    /// Read-only aggregates deliberately expose no generic write projection.
    fn values(&self) -> Result<EntityValues> {
        Err(crate::Error::InvalidRequest(
            "entity writes require its aggregate transaction".into(),
        ))
    }
}

// The declaration is the single field list. SQLx owns row decoding; these
// rules reuse its rename/flatten/json annotations for prepared writes.
macro_rules! repository_entity {
    ($table:literal, $mode:ident; keys: [$($key:literal),*], immutable: [$($immutable:literal),*], revision: $revision:expr; $($body:tt)*) => {
        $crate::storage::repository_entity!(@declare $table, $mode, [$($key),*], [$($immutable),*], $revision; $($body)*);
    };
    ($table:literal, $mode:ident; $($body:tt)*) => {
        $crate::storage::repository_entity!(@declare $table, $mode, [], [], None; $($body)*);
    };
    (@declare $table:literal, $mode:ident, [$($key:literal),*], [$($immutable:literal),*], $revision:expr;
        $(#[$attr:meta])* $vis:vis struct $entity:ident {
        $($(#[$($field_attr:tt)*])* $field_vis:vis $field:ident: $ty:ty),* $(,)?
    }) => {
        $(#[$attr])* $vis struct $entity { $($(#[$($field_attr)*])* $field_vis $field: $ty),* }
        impl $crate::storage::RepositoryEntity for $entity {
            const TABLE: &'static str = $table;
            const WRITE_MODE: $crate::storage::EntityWriteMode = $crate::storage::EntityWriteMode::$mode;
            const KEYS: &'static [&'static str] = &[$($key),*];
            const IMMUTABLE: &'static [&'static str] = &[$($immutable),*];
            const REVISION: Option<&'static str> = $revision;
            const FIELDS: &'static [$crate::storage::EntityField] = &[
                $($crate::storage::repository_entity!(@field $ty, $field; $(#[$($field_attr)*])*)),*
            ];
            const COLUMNS: &'static [&'static str] = &$crate::storage::EntityField::columns::<{ $crate::storage::EntityField::count(Self::FIELDS) }>(Self::FIELDS);
            $crate::storage::repository_entity!(@values $mode; $($field: $ty [$(#[$($field_attr)*])*]),*);
        }
    };
    (@field $ty:ty, $field:ident; #[sqlx(flatten)] $($rest:tt)*) => {
        $crate::storage::EntityField::Flatten(<$ty as $crate::storage::RepositoryEntity>::FIELDS)
    };
    (@field $ty:ty, $field:ident; #[sqlx(rename = $column:literal $(,$($extra:tt)*)?)] $($rest:tt)*) => {
        $crate::storage::EntityField::Column($column)
    };
    (@field $ty:ty, $field:ident; #[$($attr:tt)*] $($rest:tt)*) => {
        $crate::storage::repository_entity!(@field $ty, $field; $($rest)*)
    };
    (@field $ty:ty, $field:ident;) => { $crate::storage::EntityField::Column(stringify!($field)) };
    (@values ReadOnly; $($rest:tt)*) => {};
    (@values $mode:ident; $($field:ident: $ty:ty [$($attrs:tt)*]),*) => {
        fn values(&self) -> $crate::Result<$crate::storage::EntityValues> {
            let mut values = $crate::storage::EntityValues::new();
            $($crate::storage::repository_entity!(@value values, self, $ty, $field; [$($attrs)*] $($attrs)*);)*
            Ok(values)
        }
    };
    (@value $values:ident, $this:ident, $ty:ty, $field:ident; [$($all:tt)*] #[sqlx(flatten)] $($rest:tt)*) => {
        $values.extend(<$ty as $crate::storage::RepositoryEntity>::values(&$this.$field)?);
    };
    (@value $values:ident, $this:ident, $ty:ty, $field:ident; [$($all:tt)*] #[sqlx(json)] $($rest:tt)*) => {
        if let $crate::storage::EntityField::Column(column) = $crate::storage::repository_entity!(@field $ty, $field; $($all)*) {
            $values.insert(column, $crate::storage::EntityValue::json(&$this.$field)?);
        }
    };
    (@value $values:ident, $this:ident, $ty:ty, $field:ident; [$($all:tt)*] #[$($attr:tt)*] $($rest:tt)*) => {
        $crate::storage::repository_entity!(@value $values, $this, $ty, $field; [$($all)*] $($rest)*);
    };
    (@value $values:ident, $this:ident, $ty:ty, $field:ident; [$($all:tt)*]) => {
        if let $crate::storage::EntityField::Column(column) = $crate::storage::repository_entity!(@field $ty, $field; $($all)*) {
            $values.insert(column, $crate::storage::EntityValue::from(&$this.$field));
        }
    };
}
pub(crate) use repository_entity;

#[cfg(test)]
#[path = "../../tests/storage/entity_test.rs"]
mod tests;
