use crate::config::SqliteStorageConfig;
use crate::storage::base_sqlite::SqliteRepository;
use crate::storage::payment::sqlite::PaymentSqliteRepository;
use crate::storage::IIntentRepository;
use crate::storage::ReconciliationLeaseRequest;
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
            direction: None,
            owner: String::new(),
            now: 1,
            lease_until: 2,
            limit: 1,
        },
        ReconciliationLeaseRequest {
            direction: None,
            owner: "worker".into(),
            now: 2,
            lease_until: 2,
            limit: 1,
        },
        ReconciliationLeaseRequest {
            direction: None,
            owner: "worker".into(),
            now: 1,
            lease_until: 2,
            limit: 0,
        },
    ] {
        assert!(repository.claim_reconciliation(&request).await.is_err());
    }
}
