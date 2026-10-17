use super::*;

#[test]
fn expiration_summary_is_explicit_and_bounded() {
    assert_eq!(ApprovalExpirationRun::default(), ApprovalExpirationRun { expired: 0 });
    assert_eq!(MAX_BATCH_SIZE, 10_000);
}
