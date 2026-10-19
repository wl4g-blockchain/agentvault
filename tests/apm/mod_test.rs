use super::*;

#[test]
fn apm_facade_exposes_a_live_runtime_snapshot() {
    let snapshot = runtime_snapshot();
    assert_eq!(snapshot.pid, std::process::id());
    assert!(snapshot.logical_cpus > 0);
}
