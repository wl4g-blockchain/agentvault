use super::*;
#[test]
fn reconciliation_workers_have_distinct_replica_lease_identities() {
    let first = RuntimeJobFactory::reconciliation_worker_id("agentvault");
    let second = RuntimeJobFactory::reconciliation_worker_id("agentvault");

    assert_ne!(first, second);
    assert!(first.starts_with("agentvault-reconciliation-"));
    assert!(first.len() <= 128);
}
