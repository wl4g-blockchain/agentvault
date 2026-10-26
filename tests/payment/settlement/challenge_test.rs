use super::*;
#[test]
fn challenge_request_rejects_unbounded_replay_identity() {
    let command = IssuePaymentChallenge {
        scenario_id: "scenario".into(),
        run_id: "x".repeat(129),
        price_id: "price".into(),
        resource: "https://merchant.test/resource".into(),
        origin: "https://merchant.test".into(),
        protocol: "x402".into(),
        request_hash: "hash".into(),
    };
    assert!(SettlementFacts::validate_issue_command(&command).is_err());
}
