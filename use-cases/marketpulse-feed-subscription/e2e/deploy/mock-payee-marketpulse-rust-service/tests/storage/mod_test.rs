#[test]
fn storage_module_exports_sqlite_feed_storage() {
    let _ = std::any::TypeId::of::<super::feed::FeedSQLiteStorage>();
}
