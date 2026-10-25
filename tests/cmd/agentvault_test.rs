use super::*;

#[test]
fn server_can_be_constructed_only_from_typed_configuration() {
    let server = AgentVaultServer::new(AppConfig::default());
    assert!(std::mem::size_of_val(&server) > 0);
}

#[tokio::test]
async fn supervisor_rejects_an_empty_runtime() {
    let error = ServerSupervisor::new(std::time::Duration::from_secs(1))
        .run()
        .await
        .unwrap_err();
    assert!(error.to_string().contains("no runtime component"));
}

#[test]
fn durations_convert_to_bounded_database_seconds() {
    assert_eq!(
        payment_runtime::seconds(std::time::Duration::from_secs(17), "interval").unwrap(),
        17
    );
}

#[test]
fn reconciliation_workers_have_distinct_replica_lease_identities() {
    let first = payment_runtime::reconciliation_worker_id("agentvault");
    let second = payment_runtime::reconciliation_worker_id("agentvault");

    assert_ne!(first, second);
    assert!(first.starts_with("agentvault-reconciliation-"));
    assert!(first.len() <= 128);
}
