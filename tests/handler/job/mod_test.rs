use super::*;

#[test]
fn job_facade_exposes_bounded_run_summaries() {
    assert_eq!(ApprovalExpirationRun::default().expired, 0);
    assert_eq!(ReconciliationRun::default().claimed, 0);
}
