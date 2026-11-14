use super::*;
#[test]
fn grant_expiry_never_uses_the_longer_request_ttl() {
    assert_eq!(ApprovalWorkflow::grant_expiration(1_000, 100, 300), 400);
    assert_eq!(ApprovalWorkflow::grant_expiration(200, 100, 300), 200);
}
