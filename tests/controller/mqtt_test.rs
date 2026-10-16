use super::*;
use crate::storage::OutboxStatus;

fn request_body() -> Value {
    json!({
        "scenario_id": "scenario-quantstream",
        "run_id": "run-42",
        "idempotency_key": "run-42:realtime-news",
        "wallet_id": "wallet-buyer",
        "protocol": "x402",
        "asset": {
            "network": "eip155:31337",
            "asset": "USDC",
            "token_contract": "0x0000000000000000000000000000000000001001",
            "decimals": 6
        },
        "amount": "1000000",
        "recipient": "0x0000000000000000000000000000000000002002",
        "resource": "mcp://marketpulse/realtime_news",
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

fn request_envelope(body: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "type": "payment.authorize",
        "version": 1,
        "correlationId": "0123456789abcdef0123456789abcdef",
        "request": {
            "method": "POST",
            "path": AUTHORIZE_PATH,
            "headers": {
                "accept": "application/json",
                "content-type": "application/json",
                "idempotency-key": "run-42:realtime-news"
            },
            "body": URL_SAFE_NO_PAD.encode(serde_json::to_vec(body).unwrap())
        }
    }))
    .unwrap()
}

fn decision_envelope(path_approval_id: &str, body: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "type": "payment.approval_decision",
        "version": 1,
        "correlationId": "abcdef0123456789abcdef0123456789",
        "request": {
            "method": "POST",
            "path": format!("/api/v1/approvals/{path_approval_id}/decision"),
            "headers": {
                "content-type": "application/json",
                "idempotency-key": "decision-key-0001"
            },
            "body": URL_SAFE_NO_PAD.encode(serde_json::to_vec(body).unwrap())
        }
    }))
    .unwrap()
}

fn outbound_response_envelope(intent_id: &str, body: &Value) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "type": "payment.outbound_response",
        "version": 1,
        "correlationId": "fedcba9876543210fedcba9876543210",
        "request": {
            "method": "POST",
            "path": format!("/api/v1/payments/{intent_id}/outbound-response"),
            "headers": {
                "accept": "application/json",
                "content-type": "application/json",
                "idempotency-key": "run-42:realtime-news"
            },
            "body": URL_SAFE_NO_PAD.encode(serde_json::to_vec(body).unwrap())
        }
    }))
    .unwrap()
}

fn payee_envelope(message_type: &str, path: &str, body: &Value, key: Option<&str>) -> Vec<u8> {
    let mut headers = json!({
        "accept": "application/json",
        "content-type": "application/json"
    });
    if let Some(key) = key {
        headers["idempotency-key"] = Value::String(key.to_owned());
    }
    serde_json::to_vec(&json!({
        "type": message_type,
        "version": 1,
        "correlationId": "00112233445566778899aabbccddeeff",
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
fn derives_client_only_from_exact_command_topic() {
    assert!(matches!(
        command_topic(
            "agentvault/v1/clients",
            "agentvault/v1/clients/client-quantstream/commands/authorize"
        ),
        Some(CommandTopic::Authorize(client_id)) if client_id == "client-quantstream"
    ));
    assert!(matches!(
        command_topic(
            "agentvault/v1/clients",
            "agentvault/v1/clients/client-marketpulse/commands/create-challenge"
        ),
        Some(CommandTopic::CreateChallenge(client_id)) if client_id == "client-marketpulse"
    ));
    assert!(matches!(
        command_topic(
            "agentvault/v1/clients",
            "agentvault/v1/clients/client-quantstream/approvers/principal-quantstream-admin/decisions"
        ),
        Some(CommandTopic::ApprovalDecision { client_id, approver_principal })
            if client_id == "client-quantstream"
                && approver_principal == "principal-quantstream-admin"
    ));
    assert!(matches!(
        command_topic(
            "agentvault/v1/clients",
            "agentvault/v1/clients/client-quantstream/commands/outbound-response"
        ),
        Some(CommandTopic::OutboundResponse(client_id)) if client_id == "client-quantstream"
    ));
    assert!(command_topic(
        "agentvault/v1/clients",
        "agentvault/v1/clients/client-a/commands/authorize/extra"
    )
    .is_none());
    assert!(command_topic(
        "agentvault/v1/clients",
        "agentvault/v1/clients/client-a/../client-b/commands/authorize"
    )
    .is_none());
    assert!(command_topic(
        "agentvault/v1/clients",
        "agentvault/v1/clients/client-quantstream/approvers/principal/admin/decisions"
    )
    .is_none());
    assert!(command_topic(
        "agentvault/v1/clients",
        "agentvault/v1/clients/client-quantstream/approvers/principal-admin/decisions/extra"
    )
    .is_none());
}

#[test]
fn subscribes_only_to_the_six_acl_authorized_command_filters() {
    assert_eq!(
        command_subscription_filters("agentvault/v1/clients"),
        [
            "agentvault/v1/clients/+/commands/authorize",
            "agentvault/v1/clients/+/commands/outbound-response",
            "agentvault/v1/clients/+/commands/resolve-offer",
            "agentvault/v1/clients/+/commands/create-challenge",
            "agentvault/v1/clients/+/commands/verify-payment",
            "agentvault/v1/clients/+/commands/settle-payment",
        ]
    );
}

#[test]
fn payee_command_binds_wire_type_path_and_idempotency_key() {
    let body = json!({
        "scenario_id": "scenario-marketpulse",
        "run_id": "delivery-42",
        "idempotency_key": "delivery-42.challenge",
        "offer_id": "offer-realtime-news",
        "resource": "mcp://marketpulse/realtime_news",
        "origin": "http://127.0.0.1:8181",
        "protocol": "x402",
        "request_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
    });
    let parsed = parse_payee_command::<CreateChallengeRequest>(
        &payee_envelope(
            "payment.create_challenge",
            "/api/v1/payee/challenges",
            &body,
            Some("delivery-42.challenge"),
        ),
        "payment.create_challenge",
        "/api/v1/payee/challenges",
        true,
        |request| Some(request.context.idempotency_key.clone()),
    )
    .unwrap();
    assert_eq!(parsed.context.scenario_id, "scenario-marketpulse");

    assert!(parse_payee_command::<CreateChallengeRequest>(
        &payee_envelope(
            "payment.verify",
            "/api/v1/payee/challenges",
            &body,
            Some("delivery-42.challenge"),
        ),
        "payment.create_challenge",
        "/api/v1/payee/challenges",
        true,
        |request| Some(request.context.idempotency_key.clone()),
    )
    .is_err());
}

#[test]
fn isolates_approval_cache_and_result_topics_by_approver_principal() {
    let topic = command_topic(
        "agentvault/v1/clients",
        "agentvault/v1/clients/client-quantstream/approvers/principal-quantstream-admin/decisions",
    )
    .unwrap();
    assert_eq!(
        topic.cache_scope(),
        "client-quantstream:approval-decision:principal-quantstream-admin"
    );
    assert_eq!(
        topic.response_topic(
            "agentvault/v1/clients",
            "abcdef0123456789abcdef0123456789"
        ),
        "agentvault/v1/clients/client-quantstream/approvers/principal-quantstream-admin/results/abcdef0123456789abcdef0123456789"
    );
}

#[test]
fn accepts_only_canonical_bounded_authorization_envelope() {
    let body = request_body();
    let decoded = parse_authorize_command(&request_envelope(&body)).unwrap();
    assert_eq!(decoded.context.scenario_id, "scenario-quantstream");
    assert_eq!(decoded.context.idempotency_key, "run-42:realtime-news");

    let mut mismatched = body;
    mismatched["idempotency_key"] = Value::String("different-key".to_owned());
    assert_eq!(
        parse_authorize_command(&request_envelope(&mismatched)).unwrap_err(),
        "idempotency-key header and body differ"
    );
}

#[test]
fn outbound_response_command_binds_intent_run_and_idempotency() {
    let body = json!({
        "scenario_id": "scenario-quantstream",
        "run_id": "run-42",
        "idempotency_key": "run-42:realtime-news",
        "payment_response": "YWJjZA=="
    });
    let parsed = parse_outbound_response_command(&outbound_response_envelope("intent-1", &body)).unwrap();
    assert_eq!(parsed.intent_id, "intent-1");
    assert_eq!(parsed.request.context.scenario_id, "scenario-quantstream");
    assert_eq!(parsed.request.context.run_id.as_deref(), Some("run-42"));

    let mut conflicting = body;
    conflicting["idempotency_key"] = Value::String("different-key".to_owned());
    assert_eq!(
        parse_outbound_response_command(&outbound_response_envelope("intent-1", &conflicting))
            .err()
            .unwrap(),
        "idempotency-key header and body differ"
    );
    assert!(parse_outbound_response_command(&outbound_response_envelope(
        "../intent",
        &json!({
            "scenario_id": "scenario-quantstream",
            "run_id": "run-42",
            "idempotency_key": "run-42:realtime-news",
            "payment_response": "YWJjZA=="
        })
    ))
    .is_err());
}

#[test]
fn approval_decision_binds_the_topic_client_and_exact_http_shape() {
    let body = json!({
        "scenario_id": "scenario-quantstream",
        "idempotency_key": "decision-key-0001",
        "approval_id": "approval-1",
        "decision": "approve",
        "reason": "finance administrator approved"
    });
    let parsed = parse_decision_command(&decision_envelope("approval-1", &body)).unwrap();
    assert_eq!(parsed.request.context.scenario_id, "scenario-quantstream");
    assert_eq!(parsed.request.approval_id, "approval-1");

    let bad_path = decision_envelope("approval-2", &body);
    assert_eq!(parse_decision_command(&bad_path).unwrap_err().status, 400);
}

#[test]
fn rejects_padding_unknown_headers_and_noncanonical_correlation() {
    let body = request_body();
    let mut envelope: Value = serde_json::from_slice(&request_envelope(&body)).unwrap();
    envelope["request"]["body"] = Value::String(format!("{}=", envelope["request"]["body"].as_str().unwrap()));
    assert!(parse_authorize_command(&serde_json::to_vec(&envelope).unwrap()).is_err());

    let mut envelope: Value = serde_json::from_slice(&request_envelope(&body)).unwrap();
    envelope["request"]["headers"]["authorization"] = Value::String("Bearer secret".to_owned());
    assert!(parse_authorize_command(&serde_json::to_vec(&envelope).unwrap()).is_err());

    let mut envelope: Value = serde_json::from_slice(&request_envelope(&body)).unwrap();
    envelope["correlationId"] = Value::String("ABCDEF0123456789ABCDEF0123456789".to_owned());
    assert!(parse_authorize_command(&serde_json::to_vec(&envelope).unwrap()).is_err());
}

#[tokio::test]
async fn coalesces_exact_duplicates_and_conflicts_on_reuse() {
    let backend = Arc::new(crate::cache::MemoryCache::new(16, 16 * 1024, RESPONSE_CACHE_TTL).unwrap());
    backend.init().await.unwrap();
    let cache = ResponseCache::new(backend);
    let first_hash = digest(b"first");
    assert!(matches!(
        cache
            .begin("client-a", "0123456789abcdef0123456789abcdef", first_hash)
            .await,
        CacheDisposition::New
    ));
    assert!(matches!(
        cache
            .begin("client-a", "0123456789abcdef0123456789abcdef", first_hash)
            .await,
        CacheDisposition::InFlight
    ));
    let response = error_response("0123456789abcdef0123456789abcdef", 400, "INVALID_REQUEST", "invalid");
    cache
        .complete("client-a", "0123456789abcdef0123456789abcdef", response)
        .await;
    assert!(matches!(
        cache
            .begin("client-a", "0123456789abcdef0123456789abcdef", first_hash)
            .await,
        CacheDisposition::Cached(_)
    ));
    assert!(matches!(
        cache
            .begin("client-a", "0123456789abcdef0123456789abcdef", digest(b"different"))
            .await,
        CacheDisposition::Conflict
    ));
}

#[test]
fn maps_only_approval_required_outbox_destinations() {
    let message = OutboxMessage {
        id: "outbox-1".to_owned(),
        aggregate_type: "payment_intent".to_owned(),
        aggregate_id: "intent-1".to_owned(),
        event_type: "approval_required".to_owned(),
        destination: "client:client-quantstream.approvals".to_owned(),
        correlation_id: "intent-1".to_owned(),
        idempotency_key: "approval-required:approval-1".to_owned(),
        payload: json!({
            "approval_id": "approval-1",
            "intent_id": "intent-1",
            "scenario_id": "scenario-quantstream",
            "amount": "50000000",
            "expires_at": 120
        }),
        status: OutboxStatus::Pending,
        attempt_count: 1,
        available_at: 10,
        lease_owner: Some("mqtt:agentvault".to_owned()),
        lease_expires_at: Some(40),
        last_error: None,
        created_at: 10,
        delivered_at: None,
    };
    assert_eq!(
        approval_destination_client(&message).as_deref(),
        Some("client-quantstream")
    );
    let envelope = serde_json::to_value(approval_envelope(&message).unwrap()).unwrap();
    assert_eq!(envelope["type"], "payment.approval_required");
    assert_eq!(envelope["scenarioId"], "scenario-quantstream");
    assert_eq!(envelope["requestedAmount"], "50000000");

    let mut foreign = message;
    foreign.event_type = "payment_settled".to_owned();
    foreign.destination = "scenario:scenario-1.payments".to_owned();
    assert!(approval_destination_client(&foreign).is_none());
}

#[test]
fn payment_event_routing_is_tenant_scoped_and_idempotent() {
    let message = OutboxMessage {
        id: "outbox-event-1".to_owned(),
        aggregate_type: "payment_intent".to_owned(),
        aggregate_id: "intent-1".to_owned(),
        event_type: "payment_settled".to_owned(),
        destination: "scenario:scenario-quantstream.payments".to_owned(),
        correlation_id: "intent-1".to_owned(),
        idempotency_key: "settled:intent-1:tx-1".to_owned(),
        payload: json!({"intent_id": "intent-1", "tx_hash": "0x1234"}),
        status: OutboxStatus::Pending,
        attempt_count: 1,
        available_at: 10,
        lease_owner: Some("mqtt:agentvault:payment-events".to_owned()),
        lease_expires_at: Some(40),
        last_error: None,
        created_at: 10,
        delivered_at: None,
    };
    assert!(matches!(
        payment_event_destination(&message.destination),
        Ok(PaymentEventDestination::Scenario("scenario-quantstream"))
    ));
    assert!(matches!(
        payment_event_destination("client:client-quantstream.payments"),
        Ok(PaymentEventDestination::Client("client-quantstream"))
    ));
    assert!(payment_event_destination("approval").is_err());
    assert!(payment_event_destination("scenario:../client.payments").is_err());

    let envelope = serde_json::to_value(payment_event_envelope(&message).unwrap()).unwrap();
    assert_eq!(envelope["type"], "payment.event");
    assert_eq!(envelope["correlationId"], "intent-1");
    assert_eq!(envelope["idempotencyKey"], "settled:intent-1:tx-1");
    assert_eq!(envelope["eventType"], "payment_settled");
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
