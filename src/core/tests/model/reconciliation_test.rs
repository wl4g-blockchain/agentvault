use super::*;

#[test]
fn reconciliation_summary_starts_without_implied_progress() {
    let run = ReconciliationRun::default();
    assert_eq!(run.claimed, 0);
    assert_eq!(run.settled, 0);
    assert_eq!(run.provider_errors, 0);
    assert_eq!(run.skipped, 0);
}
