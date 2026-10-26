use super::*;
use crate::{
    config::StorageConfig,
    model::{PaymentIntent, ReconciliationEvidence},
    Error,
};
use async_trait::async_trait;
struct BrokenClock;
impl UnixClock for BrokenClock {
    fn now(&self) -> Result<i64> {
        Err(Error::Storage("clock unavailable".into()))
    }
}
struct NoEvidence;
#[async_trait]
impl ReconciliationEvidenceSource for NoEvidence {
    async fn lookup(&self, _: &PaymentIntent, _: i64) -> Result<ReconciliationEvidence> {
        panic!("must not query evidence before obtaining trusted time")
    }
}
#[tokio::test]
async fn scheduling_wrapper_propagates_clock_failure_before_claiming_work() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = StorageConfig::default();
    config.sqlite.database_file = directory.path().join("reconciliation.sqlite3").display().to_string();
    let provider = crate::storage::StorageFactory::new().create(&config).await.unwrap();
    let worker = ReconciliationJob::with_clock(
        provider.clone().ports().payments,
        Arc::new(NoEvidence),
        Arc::new(BrokenClock),
        ReconciliationConfig {
            worker_id: "worker".into(),
            batch_size: 1,
            lease_duration: std::time::Duration::from_secs(30),
            initial_backoff: std::time::Duration::from_secs(1),
            max_backoff: std::time::Duration::from_mins(1),
        },
    )
    .unwrap();
    assert!(worker
        .run_once()
        .await
        .unwrap_err()
        .to_string()
        .contains("clock unavailable"));
    provider.close().await.unwrap();
}
