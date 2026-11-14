use super::*;
#[test]
fn reconciliation_workers_have_distinct_replica_lease_identities() {
    let first = RuntimeJobFactory::reconciliation_worker_id("agentvault");
    let second = RuntimeJobFactory::reconciliation_worker_id("agentvault");

    assert_ne!(first, second);
    assert!(first.starts_with("agentvault-reconciliation-"));
    assert!(first.len() <= 128);
}

struct PendingEvidence;
#[async_trait::async_trait]
impl ReconciliationEvidenceSource for PendingEvidence {
    async fn lookup(
        &self,
        _intent: &crate::model::PaymentIntent,
        _at: i64,
    ) -> crate::Result<crate::model::ReconciliationEvidence> {
        Ok(crate::model::ReconciliationEvidence::Pending)
    }
}
#[tokio::test]
async fn independent_service_workers_claim_only_their_payment_direction() {
    let directory = tempfile::tempdir().unwrap();
    let mut config = AppConfig::default();
    config.storage.sqlite.database_file = directory
        .path()
        .join("role-reconciliation.sqlite3")
        .display()
        .to_string();
    let storage = crate::storage::StorageFactory::new()
        .create(&config.storage)
        .await
        .unwrap();
    let fixture = crate::storage::schema_tests::RepositoryFixture::create(storage.clone()).await;
    fixture.challenge().await;
    config.agentvault.payer.enabled = true;
    config.agentvault.payee.enabled = false;
    let outbound_worker =
        RuntimeJobFactory::build_reconciliation(&config, fixture.ports.payments.clone(), Arc::new(PendingEvidence))
            .unwrap()
            .unwrap();
    assert_eq!(outbound_worker.job.run_once().await.unwrap().claimed, 0);
    config.agentvault.payer.enabled = false;
    config.agentvault.payee.enabled = true;
    let inbound_worker =
        RuntimeJobFactory::build_reconciliation(&config, fixture.ports.payments.clone(), Arc::new(PendingEvidence))
            .unwrap()
            .unwrap();
    assert_eq!(inbound_worker.job.run_once().await.unwrap().claimed, 1);
    storage.close().await.unwrap();
}
