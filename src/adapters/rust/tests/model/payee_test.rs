use super::*;

fn operation() -> PayeeOperation {
    PayeeOperation {
        scenario_id: "scenario".to_owned(),
        run_id: "run".to_owned(),
        execution_key: "run.realtime".to_owned(),
        offer_id: "offer".to_owned(),
        resource: "mcp://marketpulse/realtime".to_owned(),
        origin: "https://marketpulse.test".to_owned(),
        protocol: "x402".to_owned(),
        request_hash: "hash".to_owned(),
        credential_payload: None,
    }
}

#[test]
fn payee_phase_keys_are_bound_to_one_execution() {
    let operation = operation();
    assert!(operation.validate().is_ok());
    assert_eq!(operation.challenge_key().unwrap().as_str(), "run.realtime.challenge");
    assert_eq!(operation.settlement_key().unwrap().as_str(), "run.realtime.settle");
}
