use super::*;
use tempfile::TempDir;

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

#[test]
fn accepts_only_typed_authorization_envelopes() {
    let decoded = parse_command(&request_envelope(&request_body())).unwrap();
    assert_eq!(decoded.principal.as_str(), crate::handler::UNRESTRICTED_PRINCIPAL);
    assert_eq!(decoded.request.context.idempotency_key, "run-42:realtime-news");

    let mut digest_request: Value = serde_json::from_slice(&request_envelope(&request_body())).unwrap();
    digest_request["type"] = Value::String("sign.digest".to_owned());
    assert!(parse_command(&serde_json::to_vec(&digest_request).unwrap()).is_err());

    let mut foreign_field: Value = serde_json::from_slice(&request_envelope(&request_body())).unwrap();
    foreign_field["digest"] = Value::String("0xdeadbeef".to_owned());
    assert!(parse_command(&serde_json::to_vec(&foreign_field).unwrap()).is_err());
}

#[test]
fn rejects_idempotency_mismatch_and_noncanonical_body() {
    let mut mismatch = request_body();
    mismatch["idempotency_key"] = Value::String("different-key".to_owned());
    assert!(parse_command(&request_envelope(&mismatch)).is_err());

    let mut padded: Value = serde_json::from_slice(&request_envelope(&request_body())).unwrap();
    padded["request"]["body"] = Value::String(format!("{}=", padded["request"]["body"].as_str().unwrap()));
    assert!(parse_command(&serde_json::to_vec(&padded).unwrap()).is_err());
}

#[tokio::test]
async fn length_prefix_is_bounded_and_round_trips() {
    let (mut writer, mut reader) = tokio::io::duplex(4096);
    let expected = br#"{"type":"payment.authorize"}"#.to_vec();
    let payload = expected.clone();
    tokio::spawn(async move {
        writer
            .write_all(&u32::try_from(payload.len()).unwrap().to_be_bytes())
            .await
            .unwrap();
        writer.write_all(&payload).await.unwrap();
    });
    assert_eq!(read_frame(&mut reader).await.unwrap(), Some(expected));

    let (mut writer, mut reader) = tokio::io::duplex(16);
    writer
        .write_all(&u32::try_from(MAX_FRAME_BYTES + 1).unwrap().to_be_bytes())
        .await
        .unwrap();
    assert_eq!(
        read_frame(&mut reader).await.unwrap_err().kind(),
        io::ErrorKind::InvalidData
    );
}

#[tokio::test]
async fn stale_socket_cleanup_never_replaces_regular_files_or_live_sockets() {
    let directory = TempDir::new().unwrap();
    let regular = directory.path().join("regular");
    fs::write(&regular, b"owner data").unwrap();
    assert!(remove_stale_socket(&regular).await.is_err());
    assert_eq!(fs::read(&regular).unwrap(), b"owner data");

    let active = directory.path().join("active.sock");
    let _listener = UnixListener::bind(&active).unwrap();
    assert!(remove_stale_socket(&active).await.is_err());
    assert!(fs::symlink_metadata(&active).unwrap().file_type().is_socket());
}

#[tokio::test]
async fn bound_socket_has_private_mode_and_guard_respects_replacement() {
    let directory = TempDir::new().unwrap();
    let path = directory.path().join("agentvault.sock");
    let (listener, guard) = bind_socket(&path).await.unwrap();
    assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, SOCKET_MODE);
    drop(listener);
    fs::remove_file(&path).unwrap();
    fs::write(&path, b"replacement").unwrap();
    drop(guard);
    assert_eq!(fs::read(&path).unwrap(), b"replacement");
}

#[tokio::test]
async fn correlation_registry_replays_exact_results_and_rejects_conflicts() {
    let registry = CorrelationRegistry::new();
    let hash = [1_u8; 32];
    assert!(matches!(
        registry.begin("principal", "correlation", hash).await,
        CorrelationDisposition::New
    ));
    assert!(matches!(
        registry.begin("principal", "correlation", [2_u8; 32]).await,
        CorrelationDisposition::Conflict
    ));
    let response = error_response("correlation", 400, "INVALID_REQUEST", "invalid");
    registry.complete("principal", "correlation", response).await;
    assert!(matches!(
        registry.begin("principal", "correlation", hash).await,
        CorrelationDisposition::Cached(_)
    ));
}
