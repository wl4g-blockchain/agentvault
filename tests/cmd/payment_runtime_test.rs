use super::*;

#[test]
fn durations_convert_to_bounded_database_seconds() {
    assert_eq!(seconds(std::time::Duration::from_secs(17), "interval").unwrap(), 17);
}

#[test]
fn reconciliation_workers_have_distinct_replica_lease_identities() {
    let first = reconciliation_worker_id("agentvault");
    let second = reconciliation_worker_id("agentvault");

    assert_ne!(first, second);
    assert!(first.starts_with("agentvault-reconciliation-"));
    assert!(first.len() <= 128);
}
