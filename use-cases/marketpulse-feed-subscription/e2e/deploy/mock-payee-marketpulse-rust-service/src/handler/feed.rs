use agentvault_sdk::SettledIncomingPayment;
use axum::{
    body::{Body, Bytes},
    extract::{Extension, State},
    http::{header, HeaderMap, HeaderValue, Response, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::storage::feed::{DeliveryState, FeedSqliteRepository, StoredSettlement};

#[derive(Clone)]
pub struct FeedHandler {
    pub(crate) storage: FeedSqliteRepository,
    allow_failure_injection: bool,
}

impl FeedHandler {
    #[must_use]
    pub fn new(storage: FeedSqliteRepository, allow_failure_injection: bool) -> Self {
        Self {
            storage,
            allow_failure_injection,
        }
    }

    #[allow(clippy::unused_async, reason = "Axum handlers expose an async interface")]
    pub async fn health(State(_handler): State<Self>) -> Json<Value> {
        Json(json!({"status":"ready","service":"marketpulse-rust"}))
    }

    #[allow(clippy::unused_async, reason = "Axum handlers expose an async interface")]
    pub async fn metrics(State(_handler): State<Self>) -> impl IntoResponse {
        (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("text/plain; version=0.0.4"),
            )],
            "# HELP marketpulse_up MarketPulse process readiness.\n# TYPE marketpulse_up gauge\nmarketpulse_up 1\n",
        )
    }
}

#[derive(Debug, Deserialize)]
struct McpRequest {
    jsonrpc: String,
    #[serde(default)]
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct McpResponse {
    jsonrpc: &'static str,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<Value>,
}

pub async fn mcp(
    State(handler): State<FeedHandler>,
    payment: Option<Extension<SettledIncomingPayment>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response<Body> {
    let request = match serde_json::from_slice::<McpRequest>(&body) {
        Ok(request) if request.jsonrpc == "2.0" => request,
        _ => return response(StatusCode::BAD_REQUEST, &error(Value::Null, -32600, "invalid request")),
    };
    let id = request.id.unwrap_or(Value::Null);
    match request.method.as_str() {
        "initialize" => response(StatusCode::OK, &initialize(id, &request.params)),
        "notifications/initialized" => empty(StatusCode::ACCEPTED),
        "tools/list" => response(StatusCode::OK, &tools(id)),
        "tools/call" => call_tool(
            &handler,
            payment.map(|value| value.0),
            &headers,
            &body,
            id,
            &request.params,
        ),
        _ => response(StatusCode::NOT_FOUND, &error(id, -32601, "method not found")),
    }
}

pub(super) fn call_tool(
    handler: &FeedHandler,
    payment: Option<SettledIncomingPayment>,
    headers: &HeaderMap,
    body: &[u8],
    id: Value,
    params: &Value,
) -> Response<Body> {
    match params.get("name").and_then(Value::as_str).unwrap_or_default() {
        "marketpulse.yesterday_news" => match handler.storage.yesterday_news(20) {
            Ok(news) => response(StatusCode::OK, &tool_result(id, &news, false, None)),
            Err(message) => response(StatusCode::INTERNAL_SERVER_ERROR, &error(id, -32603, &message)),
        },
        "marketpulse.realtime_news" => paid_realtime(handler, payment, headers, body, id),
        _ => response(StatusCode::NOT_FOUND, &error(id, -32602, "unknown tool")),
    }
}

fn paid_realtime(
    handler: &FeedHandler,
    payment: Option<SettledIncomingPayment>,
    headers: &HeaderMap,
    body: &[u8],
    id: Value,
) -> Response<Body> {
    let Some(run_id) = header_text(headers, "x-run-id") else {
        return response(StatusCode::BAD_REQUEST, &error(id, -32602, "x-run-id is required"));
    };
    let request_hash = FeedSqliteRepository::request_hash(body);
    match handler.storage.delivery(run_id, &request_hash) {
        Ok(DeliveryState::Delivered { response: replay }) => return json(StatusCode::OK, replay),
        Ok(DeliveryState::Settled(settlement)) => {
            return deliver(handler, run_id, &request_hash, id, &settlement);
        }
        Ok(DeliveryState::New) => {}
        Err(message) => return response(StatusCode::CONFLICT, &error(id, -32009, &message)),
    }
    let Some(payment) = payment else {
        return response(
            StatusCode::PAYMENT_REQUIRED,
            &error(id, -32005, "settled payment is required"),
        );
    };
    let settlement = match StoredSettlement::from_payment(run_id, &payment) {
        Ok(settlement) => settlement,
        Err(message) => return response(StatusCode::CONFLICT, &error(id, -32009, &message)),
    };
    if let Err(message) = handler.storage.record_settled(run_id, &request_hash, &settlement) {
        return response(StatusCode::CONFLICT, &error(id, -32009, &message));
    }
    if handler.allow_failure_injection && header_text(headers, "x-e2e-fail-after-settlement") == Some("once") {
        if let Err(message) = handler.storage.record_failure_injection(run_id, &request_hash) {
            return text(StatusCode::INTERNAL_SERVER_ERROR, &message);
        }
        return response(
            StatusCode::SERVICE_UNAVAILABLE,
            &error(id, -32004, "injected delivery failure after settlement"),
        );
    }
    deliver(handler, run_id, &request_hash, id, &settlement)
}

fn deliver(
    handler: &FeedHandler,
    run_id: &str,
    request_hash: &str,
    id: Value,
    settlement: &StoredSettlement,
) -> Response<Body> {
    let news = match handler.storage.realtime_news(20) {
        Ok(news) => news,
        Err(message) => return text(StatusCode::INTERNAL_SERVER_ERROR, &message),
    };
    let encoded = match serde_json::to_string(&tool_result(id, &news, true, Some(&settlement.intent_id))) {
        Ok(encoded) => encoded,
        Err(error) => return text(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    };
    if let Err(message) = handler.storage.record_delivered(run_id, request_hash, &encoded) {
        return text(StatusCode::INTERNAL_SERVER_ERROR, &message);
    }
    json(StatusCode::OK, encoded)
}

fn tools(id: Value) -> McpResponse {
    result(
        id,
        json!({"tools": [
            {"name":"marketpulse.yesterday_news","description":"Free previous-day geopolitical and tariff intelligence","inputSchema":{"type":"object","additionalProperties":false}},
            {"name":"marketpulse.realtime_news","description":"Paid real-time geopolitical and tariff intelligence via x402","inputSchema":{"type":"object","additionalProperties":false}}
        ]}),
    )
}

fn initialize(id: Value, params: &Value) -> McpResponse {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .unwrap_or("2025-11-25");
    result(
        id,
        json!({
            "protocolVersion":version,
            "capabilities":{"tools":{"listChanged":false}},
            "serverInfo":{"name":"marketpulse-feed-rust","version":"0.1.0"},
            "instructions":"Use yesterday_news for free delayed data and realtime_news for paid live data."
        }),
    )
}

fn tool_result(id: Value, news: &[crate::storage::feed::NewsItem], paid: bool, intent: Option<&str>) -> McpResponse {
    let structured = json!({"news":news,"paid":paid,"intentId":intent});
    result(
        id,
        json!({"content":[{"type":"text","text":structured.to_string()}],"structuredContent":structured,"isError":false}),
    )
}

fn result(id: Value, value: Value) -> McpResponse {
    McpResponse {
        jsonrpc: "2.0",
        id,
        result: Some(value),
        error: None,
    }
}

fn error(id: Value, code: i32, message: &str) -> McpResponse {
    McpResponse {
        jsonrpc: "2.0",
        id,
        result: None,
        error: Some(json!({"code":code,"message":message})),
    }
}

fn response(status: StatusCode, value: &impl Serialize) -> Response<Body> {
    match serde_json::to_string(value) {
        Ok(body) => json(status, body),
        Err(error) => text(StatusCode::INTERNAL_SERVER_ERROR, &error.to_string()),
    }
}

pub(super) fn json(status: StatusCode, body: String) -> Response<Body> {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

pub(super) fn text(status: StatusCode, body: &str) -> Response<Body> {
    let mut response = Response::new(Body::from(body.to_owned()));
    *response.status_mut() = status;
    response
}

fn empty(status: StatusCode) -> Response<Body> {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = status;
    response
}

pub(super) fn header_text<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

#[cfg(test)]
#[path = "../../tests/handler/feed_test.rs"]
mod tests;
