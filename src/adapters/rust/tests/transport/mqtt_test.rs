use super::*;

#[test]
fn rejects_cleartext_remote_broker_and_redacts_password() {
    assert!(MqttVaultTransport::new(MqttConfig {
        host: "broker.example".to_owned(),
        port: 1883,
        tls: false,
        username: "quantstream".to_owned(),
        password: "password".to_owned(),
        connection_prefix: "quantstream-rust".to_owned(),
        vault_client_id: "client-quantstream".to_owned(),
        request_timeout: Duration::from_secs(5),
        max_response_bytes: 2 * 1024 * 1024,
    })
    .is_err());
    let transport = MqttVaultTransport::new(MqttConfig {
        host: "127.0.0.1".to_owned(),
        port: 1883,
        tls: false,
        username: "quantstream".to_owned(),
        password: "super-secret".to_owned(),
        connection_prefix: "quantstream-rust".to_owned(),
        vault_client_id: "client-quantstream".to_owned(),
        request_timeout: Duration::from_secs(5),
        max_response_bytes: 2 * 1024 * 1024,
    })
    .expect("loopback transport");
    assert!(!format!("{transport:?}").contains("super-secret"));
    let context = producer_trace_context(&std::collections::BTreeMap::new());
    assert_eq!(context.trace_id.len(), 32);
    assert_eq!(context.span_id.len(), 16);
    let expected_traceparent = format!("00-{}-{}-01", context.trace_id, context.span_id);
    assert_eq!(
        context.headers.get("traceparent").map(String::as_str),
        Some(expected_traceparent.as_str())
    );
}

#[test]
fn projects_a_child_span_into_request_span_id() {
    let parent_trace = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01";
    let context = producer_trace_context(&std::collections::BTreeMap::from([(
        "traceparent".to_owned(),
        parent_trace.to_owned(),
    )]));
    assert_eq!(context.trace_id, "0123456789abcdef0123456789abcdef");
    assert_eq!(context.parent_span_id.as_deref(), Some("0123456789abcdef"));
    assert_eq!(
        context.headers["traceparent"].split('-').nth(2),
        Some(context.span_id.as_str())
    );
}

#[test]
fn routes_outbound_evidence_to_its_distinct_command_contract() {
    let request = TransportRequest {
        method: HttpMethod::Post,
        path: "/api/v1/payments/intent-1/outbound-response".to_owned(),
        headers: std::collections::BTreeMap::new(),
        body: Vec::new(),
    };
    let operation = mqtt_operation(&request).expect("outbound evidence operation");
    assert_eq!(operation.command, "outbound-response");
    assert_eq!(operation.message_type, "payment.outbound_response");
    assert_eq!(operation.response_type, "payment.outbound_response_result");

    let mut unsafe_path = request;
    unsafe_path.path = "/api/v1/payments/../intent/outbound-response".to_owned();
    assert!(mqtt_operation(&unsafe_path).is_err());
}

#[test]
fn routes_every_payee_phase_without_changing_the_middleware_api() {
    for (path, command, request_type, response_type) in [
        (
            paths::RESOLVE_OFFER,
            "resolve-offer",
            "payment.resolve_offer",
            "payment.offer_resolved",
        ),
        (
            paths::CREATE_CHALLENGE,
            "create-challenge",
            "payment.create_challenge",
            "payment.challenge_created",
        ),
        (
            paths::VERIFY_PAYMENT,
            "verify-payment",
            "payment.verify",
            "payment.verification_result",
        ),
        (
            paths::SETTLE_PAYMENT,
            "settle-payment",
            "payment.settle",
            "payment.settlement_result",
        ),
    ] {
        let operation = mqtt_operation(&TransportRequest {
            method: HttpMethod::Post,
            path: path.to_owned(),
            headers: std::collections::BTreeMap::new(),
            body: Vec::new(),
        })
        .expect("supported payee operation");
        assert_eq!(operation.command, command);
        assert_eq!(operation.message_type, request_type);
        assert_eq!(operation.response_type, response_type);
    }
}

#[test]
fn accepts_only_standard_error_type_for_failed_operations() {
    assert!(response_type_matches(
        "payment.authorization_result",
        200,
        "payment.authorization_result"
    ));
    assert!(response_type_matches(
        "payment.error",
        403,
        "payment.authorization_result"
    ));
    assert!(!response_type_matches(
        "payment.authorization_result",
        403,
        "payment.authorization_result"
    ));
    assert!(!response_type_matches(
        "payment.error",
        200,
        "payment.authorization_result"
    ));
}

#[test]
fn requires_an_exact_qos_one_subscription_grant() {
    assert!(subscription_granted(&SubAck::new(
        1,
        vec![SubscribeReasonCode::Success(QoS::AtLeastOnce)]
    )));
    assert!(!subscription_granted(&SubAck::new(
        1,
        vec![SubscribeReasonCode::Failure]
    )));
    assert!(!subscription_granted(&SubAck::new(
        1,
        vec![SubscribeReasonCode::Success(QoS::AtMostOnce)]
    )));
    assert!(!subscription_granted(&SubAck::new(
        1,
        vec![
            SubscribeReasonCode::Success(QoS::AtLeastOnce),
            SubscribeReasonCode::Success(QoS::AtLeastOnce),
        ]
    )));
}
