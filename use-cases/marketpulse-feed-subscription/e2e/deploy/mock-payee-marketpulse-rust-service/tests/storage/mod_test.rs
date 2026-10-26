#[test]
fn storage_module_exports_sqlite_feed_repository() {
    let _ = std::any::TypeId::of::<super::feed::FeedSqliteRepository>();
}
