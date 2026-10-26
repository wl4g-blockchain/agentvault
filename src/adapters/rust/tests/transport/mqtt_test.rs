use super::*;
use prost::bytes::BytesMut;
use rumqttc::mqttbytes::v4::{ConnAck, ConnectReturnCode, Packet, PubAck};
use std::collections::{BTreeMap, BTreeSet};
use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};
use tokio::net::{TcpListener, TcpStream};

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
    let cloned = transport.clone();
    assert!(std::sync::Arc::ptr_eq(&transport.runtime, &cloned.runtime));
    assert!(!format!("{transport:?}").contains("super-secret"));
    let context = ProducerTraceContext::new(&std::collections::BTreeMap::new());
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
    let context = ProducerTraceContext::new(&std::collections::BTreeMap::from([(
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
    let operation = MqttOperation::for_request(&request).expect("outbound evidence operation");
    assert_eq!(operation.command, "outbound-response");
    assert_eq!(operation.message_type, "payment.outbound_response");
    assert_eq!(operation.response_type, "payment.outbound_response_result");

    let mut unsafe_path = request;
    unsafe_path.path = "/api/v1/payments/../intent/outbound-response".to_owned();
    assert!(MqttOperation::for_request(&unsafe_path).is_err());
}

#[test]
fn routes_every_payee_phase_without_changing_the_middleware_api() {
    for (path, command, request_type, response_type) in [
        (
            paths::RESOLVE_PRICE,
            "resolve-price",
            "payment.resolve_price",
            "payment.price_resolved",
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
        let operation = MqttOperation::for_request(&TransportRequest {
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
    assert!(MqttRuntimeState::response_type_matches(
        "payment.authorization_result",
        200,
        "payment.authorization_result"
    ));
    assert!(MqttRuntimeState::response_type_matches(
        "payment.error",
        403,
        "payment.authorization_result"
    ));
    assert!(!MqttRuntimeState::response_type_matches(
        "payment.authorization_result",
        403,
        "payment.authorization_result"
    ));
    assert!(!MqttRuntimeState::response_type_matches(
        "payment.error",
        200,
        "payment.authorization_result"
    ));
}

#[test]
fn requires_an_exact_qos_one_subscription_grant() {
    assert!(MqttRuntimeState::subscription_granted(&SubAck::new(
        1,
        vec![SubscribeReasonCode::Success(QoS::AtLeastOnce)]
    )));
    assert!(!MqttRuntimeState::subscription_granted(&SubAck::new(
        1,
        vec![SubscribeReasonCode::Failure]
    )));
    assert!(!MqttRuntimeState::subscription_granted(&SubAck::new(
        1,
        vec![SubscribeReasonCode::Success(QoS::AtMostOnce)]
    )));
    assert!(!MqttRuntimeState::subscription_granted(&SubAck::new(
        1,
        vec![
            SubscribeReasonCode::Success(QoS::AtLeastOnce),
            SubscribeReasonCode::Success(QoS::AtLeastOnce),
        ]
    )));
}

struct MqttPeer {
    socket: TcpStream,
    buffer: BytesMut,
}

impl MqttPeer {
    async fn read(&mut self) -> anyhow::Result<Packet> {
        loop {
            match Packet::read(&mut self.buffer, 65536) {
                Ok(packet) => return Ok(packet),
                Err(rumqttc::mqttbytes::Error::InsufficientBytes(_)) => {
                    anyhow::ensure!(self.socket.read_buf(&mut self.buffer).await? > 0, "peer disconnected");
                }
                Err(error) => anyhow::bail!("invalid MQTT packet: {error}"),
            }
        }
    }

    async fn send(&mut self, packet: Packet) -> anyhow::Result<()> {
        let mut buffer = BytesMut::new();
        packet.write(&mut buffer, 65536)?;
        self.socket.write_all(&buffer).await?;
        Ok(())
    }

    async fn commands(&mut self) -> anyhow::Result<(String, BTreeMap<String, Vec<u8>>)> {
        let Packet::Connect(connect) = self.read().await? else {
            anyhow::bail!("missing CONNECT")
        };
        self.send(Packet::ConnAck(ConnAck::new(ConnectReturnCode::Success, false)))
            .await?;
        let mut commands = BTreeMap::new();
        let mut subscribed = false;
        while commands.len() < 2 || !subscribed {
            match self.read().await? {
                Packet::Subscribe(subscription) => {
                    assert_eq!(subscription.filters.len(), 1);
                    assert_eq!(subscription.filters[0].qos, QoS::AtLeastOnce);
                    self.send(Packet::SubAck(SubAck::new(
                        subscription.pkid,
                        vec![SubscribeReasonCode::Success(QoS::AtLeastOnce)],
                    )))
                    .await?;
                    subscribed = true;
                }
                Packet::Publish(command) => {
                    assert_eq!(command.qos, QoS::AtLeastOnce);
                    assert!(!command.retain);
                    let envelope: serde_json::Value = serde_json::from_slice(&command.payload)?;
                    let span = envelope["requestSpanId"].as_str().expect("span");
                    assert_eq!(
                        envelope["request"]["headers"]["traceparent"]
                            .as_str()
                            .expect("trace")
                            .split('-')
                            .nth(2),
                        Some(span)
                    );
                    commands.insert(span.to_owned(), command.payload.to_vec());
                    self.send(Packet::PubAck(PubAck::new(command.pkid))).await?;
                }
                packet => anyhow::bail!("unexpected packet: {packet:?}"),
            }
        }
        Ok((connect.client_id, commands))
    }

    async fn run(listener: TcpListener) -> anyhow::Result<()> {
        let mut first = Self {
            socket: listener.accept().await?.0,
            buffer: BytesMut::new(),
        };
        let (client, original) = first.commands().await?;
        first.socket.shutdown().await?;
        drop(first);
        let mut second = Self {
            socket: listener.accept().await?.0,
            buffer: BytesMut::new(),
        };
        let (reconnected_client, replay) = second.commands().await?;
        assert_eq!(client, reconnected_client);
        assert_eq!(original, replay);
        for (index, (span, raw)) in original.iter().rev().enumerate() {
            let request: serde_json::Value = serde_json::from_slice(raw)?;
            let response = serde_json::to_vec(&serde_json::json!({
                "type": "payment.challenge_created", "version": 1, "requestSpanId": span,
                "status": 200, "headers": {}, "body": request["request"]["body"],
            }))?;
            let mut publish = Publish::new(
                format!("agentvault/v1/clients/client/results/{span}"),
                QoS::AtLeastOnce,
                response,
            );
            publish.pkid = u16::try_from(index + 1)?;
            second.send(Packet::Publish(publish.clone())).await?;
            publish.dup = true;
            second.send(Packet::Publish(publish)).await?;
        }
        let mut acknowledged = BTreeSet::new();
        while acknowledged.len() < 2 {
            match second.read().await? {
                Packet::PubAck(ack) => {
                    acknowledged.insert(ack.pkid);
                }
                Packet::Publish(command) => {
                    assert!(original.values().any(|raw| raw.as_slice() == command.payload.as_ref()));
                    second.send(Packet::PubAck(PubAck::new(command.pkid))).await?;
                }
                packet => anyhow::bail!("unexpected reply packet: {packet:?}"),
            }
        }
        Ok(())
    }

    fn transport(port: u16) -> MqttVaultTransport {
        MqttVaultTransport::new(MqttConfig {
            host: "127.0.0.1".into(),
            port,
            tls: false,
            username: "user".into(),
            password: "pass".into(),
            connection_prefix: "worker".into(),
            vault_client_id: "client".into(),
            request_timeout: Duration::from_secs(8),
            max_response_bytes: 65536,
        })
        .expect("transport")
    }

    fn request(body: &[u8]) -> TransportRequest {
        TransportRequest {
            method: HttpMethod::Post,
            path: paths::CREATE_CHALLENGE.into(),
            headers: BTreeMap::new(),
            body: body.to_vec(),
        }
    }
}

#[tokio::test]
async fn reconnect_replays_exact_qos_one_envelopes_and_pairs_reordered_duplicate_replies() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let transport = MqttPeer::transport(listener.local_addr().expect("address").port());
    let peer = tokio::spawn(MqttPeer::run(listener));
    let (first, second, peer) = timeout(Duration::from_secs(10), async {
        tokio::join!(
            transport.send(MqttPeer::request(b"one")),
            transport.send(MqttPeer::request(b"two")),
            peer
        )
    })
    .await
    .expect("bounded replay");
    assert_eq!(first.expect("first").body, b"one");
    assert_eq!(second.expect("second").body, b"two");
    peer.expect("join peer").expect("peer");
    transport.close().await.expect("close");
    transport.close().await.expect("idempotent close");
}

#[tokio::test]
async fn cancellation_and_close_remove_pending_without_waiting_for_the_broker() {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let transport = MqttPeer::transport(listener.local_addr().expect("address").port());
    let cloned = transport.clone();
    let pending = tokio::spawn(async move { cloned.send(MqttPeer::request(b"cancel")).await });
    timeout(Duration::from_secs(1), async {
        loop {
            if let Some(runtime) = transport.runtime.get() {
                if runtime.state.pending.lock().expect("lock").len() == 1 {
                    break;
                }
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("registered");
    pending.abort();
    assert!(pending.await.expect_err("cancelled").is_cancelled());
    assert!(transport
        .runtime
        .get()
        .expect("runtime")
        .state
        .pending
        .lock()
        .expect("lock")
        .is_empty());
    transport.close().await.expect("close");
    assert!(transport.send(MqttPeer::request(b"closed")).await.is_err());
}
