use super::IngestionHandler;

#[tokio::test]
async fn health_identifies_the_rust_ingestor() {
    let response = IngestionHandler::health().await;
    assert_eq!(response.0["status"], "ready");
    assert_eq!(response.0["service"], "quantstream-rust");
}

#[tokio::test]
async fn metrics_reports_process_readiness() {
    use axum::response::IntoResponse as _;

    let response = IngestionHandler::metrics().await.into_response();
    assert_eq!(response.status(), axum::http::StatusCode::OK);
}
