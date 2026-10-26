use crate::{
    config::SqliteStorageConfig,
    storage::{
        payment::sqlite::PaymentSqliteRepository, sqlite::SqliteRepository, IIntentRepository,
        ReconciliationLeaseRequest,
    },
};
#[tokio::test]
async fn invalid_lease_requests_do_not_start_claims() {
    let directory = tempfile::tempdir().unwrap();
    let config = SqliteStorageConfig {
        database_file: directory.path().join("leases.sqlite").display().to_string(),
        ..SqliteStorageConfig::default()
    };
    let database = SqliteRepository::connect(&config).await.unwrap();
    let repository = PaymentSqliteRepository::new(database);
    for request in [
        ReconciliationLeaseRequest {
            owner: String::new(),
            now: 1,
            lease_until: 2,
            limit: 1,
        },
        ReconciliationLeaseRequest {
            owner: "worker".into(),
            now: 2,
            lease_until: 2,
            limit: 1,
        },
        ReconciliationLeaseRequest {
            owner: "worker".into(),
            now: 1,
            lease_until: 2,
            limit: 0,
        },
    ] {
        assert!(repository.claim_reconciliation(&request).await.is_err());
    }
}
