use super::super::tests::*;
use super::*;
#[test]
fn payee_command_binds_wire_type_path_and_run_id() {
    let body = json!({
        "scenario_id": "scenario-marketpulse",
        "run_id": "delivery-42",
        "offer_id": "offer-realtime-news",
        "resource": "mcp://marketpulse/realtime_news",
        "origin": "http://127.0.0.1:8181",
        "protocol": "x402",
        "request_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    });
    let parsed = MqttWireCodec::parse_payee_command::<CreateChallengeRequest>(
        &payee_envelope("payment.create_challenge", "/api/v1/payee/challenges", &body),
        "payment.create_challenge",
        "/api/v1/payee/challenges",
    )
    .unwrap();
    assert_eq!(parsed.context.scenario_id, "scenario-marketpulse");
    assert_eq!(parsed.context.run_id, "delivery-42");

    assert!(MqttWireCodec::parse_payee_command::<CreateChallengeRequest>(
        &payee_envelope("payment.verify", "/api/v1/payee/challenges", &body),
        "payment.create_challenge",
        "/api/v1/payee/challenges",
    )
    .is_err());
}
