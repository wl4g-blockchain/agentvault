use crate::storage::base_postgres::PostgresRepository;
use crate::storage::base_sqlite::SqliteRepository;
use crate::storage::schema_tests::CatalogBook;
use crate::storage::EntityComparison;
use crate::storage::EntityQuery;
use crate::storage::RepositorySql;
#[test]
fn query_builder_rejects_unknown_columns_and_unsafe_identifiers() {
    for field in ["not_a_column", "id; DELETE FROM repository_catalog_book"] {
        let query = EntityQuery::new().eq(field, "book");
        assert!(<PostgresRepository as RepositorySql>::select::<CatalogBook>(&query, false).is_err());
        assert!(<SqliteRepository as RepositorySql>::select::<CatalogBook>(&query, false).is_err());
    }
    let invalid = EntityQuery::new().compare("borrowed_at", EntityComparison::Greater, Option::<i64>::None);
    assert!(<SqliteRepository as RepositorySql>::select::<CatalogBook>(&invalid, false).is_err());
    let null = EntityQuery::new().eq("borrowed_at", Option::<i64>::None);
    assert!(
        <PostgresRepository as RepositorySql>::select::<CatalogBook>(&null, false)
            .unwrap()
            .sql()
            .contains("borrowed_at IS NULL")
    );
}
