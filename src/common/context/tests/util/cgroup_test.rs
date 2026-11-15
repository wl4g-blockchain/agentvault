use common_context::util::cgroup::Cgroup;

#[test]
fn reports_positive_process_limits() {
    assert!(Cgroup::cpu_limit() > 0);
    assert!(Cgroup::memory_limit() > 0);
}
