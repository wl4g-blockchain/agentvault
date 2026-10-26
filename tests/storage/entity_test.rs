use super::*;
use crate::storage::schema_tests::CatalogBook;
#[test]
fn entity_metadata_does_not_require_serde() {
    assert_eq!(CatalogBook::TABLE, "repository_catalog_book");
    assert_eq!(
        CatalogBook::columns(),
        &["id", "title", "copies", "available", "borrowed_at"]
    );
    let book = CatalogBook {
        id: "book".into(),
        title: String::new(),
        copies: 0,
        available: false,
        borrowed_at: None,
    };
    let values = book.values().unwrap();
    assert!(matches!(values["copies"], EntityValue::Integer(Some(0))));
    assert!(matches!(values["available"], EntityValue::Bool(Some(false))));
    assert!(matches!(values["borrowed_at"], EntityValue::Integer(None)));
}
