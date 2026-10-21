use super::*;

#[test]
fn rejects_unsafe_endpoint_limits_and_reserved_trace_headers() {
    assert!(client(
        "http://example.com",
        Duration::from_secs(1),
        1024,
        &BTreeMap::new(),
        true,
    )
    .is_err());
    assert!(client(
        "http://127.0.0.1:8899",
        Duration::from_secs(1),
        0,
        &BTreeMap::new(),
        true,
    )
    .is_err());
    for name in ["traceparent", "tracestate"] {
        assert!(client(
            "https://rpc.example",
            Duration::from_secs(1),
            1024,
            &BTreeMap::from([(name.to_owned(), "operator-value".to_owned())]),
            false,
        )
        .is_err());
    }
}

#[tokio::test]
async fn rejects_oversized_chunked_rpc_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let router = axum::Router::new().route(
        "/",
        axum::routing::post(|| async {
            axum::Json(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 0,
                "result": "x".repeat(2048),
            }))
        }),
    );
    let server = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let rpc = client(
        &format!("http://{address}"),
        Duration::from_secs(2),
        128,
        &BTreeMap::new(),
        true,
    )
    .unwrap();
    let error = rpc
        .send::<Value>(RpcRequest::GetVersion, Value::Null)
        .await
        .unwrap_err();
    assert!(error.to_string().contains("response exceeds configured limit"));
    server.abort();
    let _ = server.await;
}
