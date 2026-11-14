use super::*;
#[test]
fn verification_rejects_a_credential_bound_to_another_challenge() {
    let command = VerifyIncomingPayment {
        scenario_id: "scenario".into(),
        run_id: "run".into(),
        challenge_hash: "challenge-a".into(),
        request_hash: "hash".into(),
        credential: PaymentCredential {
            protocol: "x402".into(),
            challenge_hash: "challenge-b".into(),
            credential_hash: "credential".into(),
            payload: json!({}),
        },
    };
    assert!(matches!(
        SettlementFacts::validate_verify_command(&command),
        Err(Error::PaymentConflict(_))
    ));
}
