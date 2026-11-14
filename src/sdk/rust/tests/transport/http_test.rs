use super::*;
use axum::{extract::Request, routing::post, Json, Router};
#[tokio::test]
async fn http_sends_json_auth_and_trace_and_bounds_the_response() {
    let routes = Router::new().route(
        "/api/v1/test",
        post(|request: Request| async move {
            assert_eq!(request.headers()["authorization"], "Bearer edge-secret");
            assert!(request.headers()["traceparent"].to_str().unwrap().starts_with("00-"));
            assert_eq!(request.headers()["content-type"], "application/json");
            Json(serde_json::json!({"ok":true}))
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, routes).await.unwrap() });
    let config = HttpConfig::new(format!("http://{address}")).with_authguard_bearer("edge-secret");
    assert!(!format!("{config:?}").contains("edge-secret"));
    let transport = HttpVaultTransport::new(config.clone()).unwrap();
    let request = TransportRequest {
        method: HttpMethod::Post,
        path: "/api/v1/test".into(),
        headers: BTreeMap::from([("content-type".into(), "application/json".into())]),
        body: b"{}".to_vec(),
    };
    let response = transport.send(request.clone()).await.unwrap();
    assert_eq!(response.status, 200);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&response.body).unwrap()["ok"],
        true
    );
    let mut small = config;
    small.max_response_bytes = 4;
    let error = HttpVaultTransport::new(small)
        .unwrap()
        .send(request.clone())
        .await
        .unwrap_err();
    assert_eq!(error.kind(), crate::TransportErrorKind::ResponseTooLarge);
    transport.close().await.unwrap();
    assert!(transport.send(request).await.is_err());
    server.abort();
}
#[test]
fn http_rejects_non_http_and_credential_bearing_endpoints() {
    for url in [
        "file:///tmp/vault",
        "https://user:secret@vault.example",
        "https://vault.example/?token=secret",
    ] {
        assert!(HttpVaultTransport::new(HttpConfig::new(url)).is_err());
    }
}
