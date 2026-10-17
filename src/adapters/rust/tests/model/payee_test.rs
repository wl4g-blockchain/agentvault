use super::*;

fn operation() -> PayeeOperation {
    PayeeOperation {
        scenario_id: "scenario".to_owned(),
        run_id: "run".to_owned(),
        offer_id: "offer".to_owned(),
        resource: "mcp://marketpulse/realtime".to_owned(),
        origin: "https://marketpulse.test".to_owned(),
        protocol: "x402".to_owned(),
        request_hash: "hash".to_owned(),
        credential_payload: None,
    }
}

#[test]
fn every_payee_phase_reuses_one_run_identity() {
    let operation = operation();
    assert!(operation.validate().is_ok());
    assert_eq!(operation.run_id, "run");
}
