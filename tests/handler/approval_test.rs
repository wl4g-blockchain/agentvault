use super::*;
#[tokio::test]
async fn handler_queries_its_configured_approval_repository() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = crate::config::StorageConfig::default();
    config.sqlite.database_file = directory.path().join("approval.sqlite3").display().to_string();
    let provider = crate::storage::StorageFactory::new().create(&config).await.unwrap();
    let handler = ApprovalHandler::new(provider.clone().ports().approvals);
    assert!(handler.get("absent").await.unwrap().is_none());
    assert!(handler
        .list_pending("absent", 1, PageRequest::default())
        .await
        .unwrap()
        .items
        .is_empty());
    provider.close().await.unwrap();
}
