use super::*;

#[test]
fn snapshot_reports_the_current_process() {
    let profile = runtime_snapshot();
    assert_eq!(profile.pid, std::process::id());
    assert!(profile.logical_cpus > 0);
}
