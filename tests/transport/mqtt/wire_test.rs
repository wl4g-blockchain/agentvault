use super::super::tests::*;
use super::*;
#[test]
fn derives_client_only_from_exact_command_topic() {
    assert!(matches!(
        MqttWireCodec::command_topic(
            "agentvault/v1/clients",
            "agentvault/v1/clients/client-quantstream/commands/authorize"
        ),
        Some(CommandTopic::Authorize(client_id)) if client_id == "client-quantstream"
    ));
    assert!(matches!(
        MqttWireCodec::command_topic(
            "agentvault/v1/clients",
            "agentvault/v1/clients/client-marketpulse/commands/create-challenge"
        ),
        Some(CommandTopic::CreateChallenge(client_id)) if client_id == "client-marketpulse"
    ));
    assert!(matches!(
        MqttWireCodec::command_topic(
            "agentvault/v1/clients",
            "agentvault/v1/clients/client-quantstream/approvers/principal-quantstream-admin/decisions"
        ),
        Some(CommandTopic::ApprovalDecision { client_id, approver_principal })
            if client_id == "client-quantstream"
                && approver_principal == "principal-quantstream-admin"
    ));
    assert!(matches!(
        MqttWireCodec::command_topic(
            "agentvault/v1/clients",
            "agentvault/v1/clients/client-quantstream/commands/outbound-response"
        ),
        Some(CommandTopic::OutboundResponse(client_id)) if client_id == "client-quantstream"
    ));
    assert!(MqttWireCodec::command_topic(
        "agentvault/v1/clients",
        "agentvault/v1/clients/client-a/commands/authorize/extra"
    )
    .is_none());
    assert!(MqttWireCodec::command_topic(
        "agentvault/v1/clients",
        "agentvault/v1/clients/client-a/../client-b/commands/authorize"
    )
    .is_none());
    assert!(MqttWireCodec::command_topic(
        "agentvault/v1/clients",
        "agentvault/v1/clients/client-quantstream/approvers/principal/admin/decisions"
    )
    .is_none());
    assert!(MqttWireCodec::command_topic(
        "agentvault/v1/clients",
        "agentvault/v1/clients/client-quantstream/approvers/principal-admin/decisions/extra"
    )
    .is_none());
}

#[test]
fn isolates_approval_cache_and_result_topics_by_approver_principal() {
    let topic = MqttWireCodec::command_topic(
        "agentvault/v1/clients",
        "agentvault/v1/clients/client-quantstream/approvers/principal-quantstream-admin/decisions",
    )
    .unwrap();
    assert_eq!(
        topic.cache_scope(),
        "client-quantstream:approval-decision:principal-quantstream-admin"
    );
    assert_eq!(
        topic.response_topic("agentvault/v1/clients", "abcdef0123456789"),
        "agentvault/v1/clients/client-quantstream/approvers/principal-quantstream-admin/results/abcdef0123456789"
    );
}

#[test]
fn accepts_only_canonical_bounded_authorization_envelope() {
    let body = request_body();
    let decoded = MqttWireCodec::parse_authorize_command(&request_envelope(&body)).unwrap();
    assert_eq!(decoded.context.scenario_id, "scenario-quantstream");
    assert_eq!(decoded.context.run_id, "run-42");
}

#[test]
fn outbound_response_command_binds_intent_and_run() {
    let body = json!({
        "scenario_id": "scenario-quantstream",
        "run_id": "run-42",
        "payment_response": "YWJjZA=="
    });
    let parsed =
        MqttWireCodec::parse_outbound_response_command(&outbound_response_envelope("intent-1", &body)).unwrap();
    assert_eq!(parsed.intent_id, "intent-1");
    assert_eq!(parsed.request.context.scenario_id, "scenario-quantstream");
    assert_eq!(parsed.request.context.run_id, "run-42");

    assert!(
        MqttWireCodec::parse_outbound_response_command(&outbound_response_envelope(
            "../intent",
            &json!({
                "scenario_id": "scenario-quantstream",
                "run_id": "run-42",
                "payment_response": "YWJjZA=="
            })
        ))
        .is_err()
    );
}

#[test]
fn approval_decision_binds_the_topic_client_and_exact_http_shape() {
    let body = json!({
        "scenario_id": "scenario-quantstream",
        "run_id": "approval-run-1",
        "approval_id": "approval-1",
        "decision": "approve",
        "reason": "finance administrator approved"
    });
    let parsed = MqttWireCodec::parse_decision_command(&decision_envelope("approval-1", &body)).unwrap();
    assert_eq!(parsed.request.context.scenario_id, "scenario-quantstream");
    assert_eq!(parsed.request.approval_id, "approval-1");

    let bad_path = decision_envelope("approval-2", &body);
    assert_eq!(
        MqttWireCodec::parse_decision_command(&bad_path).unwrap_err().status,
        400
    );
}

#[test]
fn rejects_padding_unknown_headers_and_noncanonical_request_span_id() {
    let body = request_body();
    let mut envelope: Value = serde_json::from_slice(&request_envelope(&body)).unwrap();
    envelope["request"]["body"] = Value::String(format!("{}=", envelope["request"]["body"].as_str().unwrap()));
    assert!(MqttWireCodec::parse_authorize_command(&serde_json::to_vec(&envelope).unwrap()).is_err());

    let mut envelope: Value = serde_json::from_slice(&request_envelope(&body)).unwrap();
    envelope["request"]["headers"]["authorization"] = Value::String("Bearer secret".to_owned());
    assert!(MqttWireCodec::parse_authorize_command(&serde_json::to_vec(&envelope).unwrap()).is_err());

    let mut envelope: Value = serde_json::from_slice(&request_envelope(&body)).unwrap();
    envelope["requestSpanId"] = Value::String("ABCDEF0123456789".to_owned());
    assert!(MqttWireCodec::parse_authorize_command(&serde_json::to_vec(&envelope).unwrap()).is_err());

    let mut envelope: Value = serde_json::from_slice(&request_envelope(&body)).unwrap();
    envelope["request"]["headers"]["traceparent"] =
        Value::String("00-11111111111111111111111111111111-fedcba9876543210-01".to_owned());
    assert!(MqttWireCodec::parse_authorize_command(&serde_json::to_vec(&envelope).unwrap()).is_err());
}
