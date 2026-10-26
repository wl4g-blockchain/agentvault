use super::*;
pub(super) fn request_body() -> Value {
    json!({
        "scenario_id": "scenario-quantstream",
        "run_id": "run-42",
        "challenge": {
            "protocol_hint": "x402",
            "origin": "http://127.0.0.1:8181",
            "resource": "mcp://marketpulse/realtime_news",
            "content_type": "application/json",
            "headers": {"payment-required": "opaque"},
            "body": "opaque"
        }
    })
}

pub(super) fn request_envelope(body: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "type": "payment.authorize",
        "version": 1,
        "requestSpanId": "0123456789abcdef",
        "request": {
            "method": "POST",
            "path": AUTHORIZE_PATH,
            "headers": {
                "accept": "application/json",
                "content-type": "application/json",
                "traceparent": "00-11111111111111111111111111111111-0123456789abcdef-01"
            },
            "body": URL_SAFE_NO_PAD.encode(serde_json::to_vec(body).unwrap())
        }
    }))
    .unwrap()
}

pub(super) fn decision_envelope(path_approval_id: &str, body: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "type": "payment.approval_decision",
        "version": 1,
        "requestSpanId": "abcdef0123456789",
        "request": {
            "method": "POST",
            "path": format!("/api/v1/approvals/{path_approval_id}/decision"),
            "headers": {
                "content-type": "application/json",
                "traceparent": "00-22222222222222222222222222222222-abcdef0123456789-01"
            },
            "body": URL_SAFE_NO_PAD.encode(serde_json::to_vec(body).unwrap())
        }
    }))
    .unwrap()
}

pub(super) fn outbound_response_envelope(intent_id: &str, body: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "type": "payment.outbound_response",
        "version": 1,
        "requestSpanId": "fedcba9876543210",
        "request": {
            "method": "POST",
            "path": format!("/api/v1/payments/{intent_id}/outbound-response"),
            "headers": {
                "accept": "application/json",
                "content-type": "application/json",
                "traceparent": "00-33333333333333333333333333333333-fedcba9876543210-01"
            },
            "body": URL_SAFE_NO_PAD.encode(serde_json::to_vec(body).unwrap())
        }
    }))
    .unwrap()
}

pub(super) fn payee_envelope(message_type: &str, path: &str, body: &Value) -> Vec<u8> {
    let headers = json!({
        "accept": "application/json",
        "content-type": "application/json",
        "traceparent": "00-44444444444444444444444444444444-0011223344556677-01"
    });
    serde_json::to_vec(&json!({
        "type": message_type,
        "version": 1,
        "requestSpanId": "0011223344556677",
        "request": {
            "method": "POST",
            "path": path,
            "headers": headers,
            "body": URL_SAFE_NO_PAD.encode(serde_json::to_vec(body).unwrap())
        }
    }))
    .unwrap()
}

#[test]
fn subscribes_only_to_the_six_shared_acl_authorized_command_filters() {
    assert_eq!(
        MqttSettings::command_subscription_filters("agentvault/v1/clients", "agentvault-pool"),
        [
            "$share/agentvault-pool/agentvault/v1/clients/+/commands/authorize",
            "$share/agentvault-pool/agentvault/v1/clients/+/commands/outbound-response",
            "$share/agentvault-pool/agentvault/v1/clients/+/commands/resolve-offer",
            "$share/agentvault-pool/agentvault/v1/clients/+/commands/create-challenge",
            "$share/agentvault-pool/agentvault/v1/clients/+/commands/verify-payment",
            "$share/agentvault-pool/agentvault/v1/clients/+/commands/settle-payment",
        ]
    );
}

#[test]
fn subscription_ack_fails_closed_on_partial_broker_authorization() {
    let accepted = SubAck::new(1, vec![SubscribeReasonCode::Success(QoS::AtLeastOnce); 7]);
    assert!(MqttSettings::validate_subscription_ack(&accepted, 7).is_ok());

    let rejected = SubAck::new(
        2,
        vec![
            SubscribeReasonCode::Success(QoS::AtLeastOnce),
            SubscribeReasonCode::Failure,
        ],
    );
    assert!(MqttSettings::validate_subscription_ack(&rejected, 2).is_err());
}

#[test]
fn debug_output_redacts_resolved_broker_password() {
    let settings = MqttSettings {
        host: "127.0.0.1".to_owned(),
        port: 1883,
        client_id: "agentvault".to_owned(),
        consumer_group: "agentvault-pool".to_owned(),
        topic_prefix: "agentvault/v1/clients".to_owned(),
        tls: false,
        username: "vault".to_owned(),
        password: Zeroizing::new("super-secret".to_owned()),
        request_timeout: Duration::from_secs(5),
        reconnect_delay: Duration::from_secs(1),
    };
    let debug = format!("{settings:?}");
    assert!(debug.contains("<redacted>"));
    assert!(!debug.contains("super-secret"));
}
