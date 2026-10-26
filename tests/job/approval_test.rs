use super::*;
#[tokio::test]
async fn expiration_worker_rejects_unbounded_batches_and_runs_an_empty_batch() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = crate::config::StorageConfig::default();
    config.sqlite.database_file = directory.path().join("expiration.sqlite3").display().to_string();
    let provider = crate::storage::StorageFactory::new().create(&config).await.unwrap();
    let repository = provider.clone().ports().approvals;
    assert!(ApprovalExpirationJob::new(repository.clone(), 0).is_err());
    assert!(ApprovalExpirationJob::new(repository.clone(), 10_001).is_err());
    assert_eq!(
        ApprovalExpirationJob::new(repository, 10)
            .unwrap()
            .run_once()
            .await
            .unwrap()
            .expired,
        0
    );
    provider.close().await.unwrap();
}
