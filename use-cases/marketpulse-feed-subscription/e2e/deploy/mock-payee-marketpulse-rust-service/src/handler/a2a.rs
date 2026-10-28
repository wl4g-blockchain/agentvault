use super::feed::{call_tool, header_text, json, text, FeedHandler};
use agentvault_sdk::{A2aCall, A2aTaskStore, SettledIncomingPayment};
use axum::{
    body::{Body, Bytes},
    extract::{Extension, State},
    http::{HeaderMap, Response, StatusCode},
};
use serde_json::{json, Value};

impl FeedHandler {
    /// Delivers the native Task using the same durable news operation as MCP.
    pub async fn a2a(
        State(handler): State<Self>,
        payment: Option<Extension<SettledIncomingPayment>>,
        headers: HeaderMap,
        body: Bytes,
    ) -> Response<Body> {
        let result = handler
            .a2a_delivery(payment.map(|value| value.0), &headers, &body)
            .await;
        match result {
            Ok(response) => response,
            Err(error) => text(StatusCode::BAD_REQUEST, &error.to_string()),
        }
    }

    async fn a2a_delivery(
        &self,
        payment: Option<SettledIncomingPayment>,
        headers: &HeaderMap,
        body: &[u8],
    ) -> Result<Response<Body>, Box<dyn std::error::Error + Send + Sync>> {
        let call = A2aCall::parse(body, 1024 * 1024)?.ok_or("message/send is required")?;
        let params = call.params()?;
        let task_id = params.message.task_id.as_deref().ok_or("payment Task is required")?;
        let task = self.storage.get(task_id).await?.ok_or("payment Task is missing")?;
        let rpc: Value = serde_json::from_slice(body)?;
        let run_id = header_text(headers, "x-run-id").ok_or("x-run-id is required")?;
        let operation = json!({"name":"marketpulse.realtime_news", "arguments":{}});
        let canonical =
            serde_json::to_vec(&json!({"jsonrpc":"2.0", "id":run_id, "method":"tools/call", "params":operation}))?;
        let delivered = call_tool(self, payment, headers, &canonical, rpc["id"].clone(), &operation);
        let status = delivered.status();
        let body = axum::body::to_bytes(delivered.into_body(), 8 * 1024 * 1024).await?;
        let business: Value = serde_json::from_slice(&body)?;
        if business.get("error").is_some() {
            return Ok(json(status, business.to_string()));
        }
        let mut task = serde_json::to_value(task)?;
        task["status"] = json!({"state":"completed"});
        task["artifacts"] = json!([{"artifactId":"realtime-news", "parts":[{"kind":"data", "data":business["result"]["structuredContent"]}]}]);
        self.storage.save(&serde_json::from_value(task.clone())?).await?;
        Ok(json(
            StatusCode::OK,
            json!({"jsonrpc":"2.0", "id":rpc["id"], "result":task}).to_string(),
        ))
    }
}

#[cfg(test)]
#[path = "../../tests/handler/a2a_test.rs"]
mod tests;
