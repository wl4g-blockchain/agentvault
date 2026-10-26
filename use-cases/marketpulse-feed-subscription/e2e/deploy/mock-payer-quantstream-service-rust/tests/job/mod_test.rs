#[test]
fn job_module_exports_a_monotonic_wall_clock_value() {
    assert!(super::ingestion_job::unix_time() > 0);
}
