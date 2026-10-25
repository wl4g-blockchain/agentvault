use axum::{response::IntoResponse, Json};
use serde_json::{json, Value};

#[derive(Clone, Copy, Default)]
pub struct IngestionHandler;

impl IngestionHandler {
    #[allow(clippy::unused_async, reason = "Axum handlers expose an async interface")]
    pub async fn health() -> Json<Value> {
        Json(json!({"status": "ready", "service": "quantstream-rust"}))
    }

    #[allow(clippy::unused_async, reason = "Axum handlers expose an async interface")]
    pub async fn metrics() -> impl IntoResponse {
        "# HELP quantstream_up QuantStream process readiness.\n# TYPE quantstream_up gauge\nquantstream_up 1\n"
    }
}

#[cfg(test)]
#[path = "../../tests/handler/ingestion_test.rs"]
mod tests;
