use std::{borrow::Cow, collections::HashMap, env, sync::Arc, time::Duration};

use agentvault_sdk::{
    AgentVault, AssetIdentity, AuthorizedPayment, MqttConfig, MqttVaultTransport, PayerConfig, PayerRequestContext,
    PayerRequestExt,
};
use futures::stream::BoxStream;
use http::{HeaderName, HeaderValue};
use reqwest_middleware::ClientWithMiddleware;
use rmcp::{
    model::{CallToolRequestParams, ClientJsonRpcMessage, JsonObject, RequestMetaObject, ServerJsonRpcMessage},
    service::RunningService,
    transport::streamable_http_client::{
        StreamableHttpClient, StreamableHttpClientTransport, StreamableHttpClientTransportConfig, StreamableHttpError,
        StreamableHttpPostResponse,
    },
    RoleClient, ServiceExt,
};
use serde_json::{json, Value};
use sse_stream::{Error as SseError, Sse};
use thiserror::Error;
use tokio::sync::Mutex;

pub type DynError = Box<dyn std::error::Error + Send + Sync>;

const MAX_MCP_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const YESTERDAY_TOOL: &str = "marketpulse.yesterday_news";
const REALTIME_TOOL: &str = "marketpulse.realtime_news";

#[derive(Clone)]
pub struct MCPHelper {
    client: Arc<RunningService<RoleClient, ()>>,
    payment_intents: Arc<Mutex<HashMap<String, String>>>,
}

pub struct ToolResult {
    pub body: Vec<u8>,
    pub payment_intent_id: Option<String>,
}

impl MCPHelper {
    /// Initializes the official MCP client, discovers the two remote tools,
    /// and installs `AgentVault`'s reqwest payer middleware into its transport.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid configuration, MCP initialization, or a
    /// `MarketPulse` server that does not expose the required tools.
    pub async fn from_env() -> Result<Self, DynError> {
        let asset = AssetIdentity {
            network: required_env("PAYMENT_NETWORK")?,
            asset: required_env("PAYMENT_ASSET")?,
            token_contract: Some(required_env("PAYMENT_TOKEN_CONTRACT")?),
            decimals: required_env("PAYMENT_DECIMALS")?.parse()?,
        };
        let mqtt = MqttVaultTransport::new(MqttConfig {
            host: required_env("AGENTVAULT_MQTT_HOST")?,
            port: required_env("AGENTVAULT_MQTT_PORT")?.parse()?,
            tls: required_env("AGENTVAULT_MQTT_TLS")?.parse()?,
            username: required_env("AGENTVAULT_MQTT_USERNAME")?,
            password: required_env("AGENTVAULT_MQTT_PASSWORD")?,
            connection_prefix: "quantstream-rust".to_owned(),
            vault_client_id: required_env("QUANTSTREAM_CLIENT_ID")?,
            request_timeout: Duration::from_secs(positive_env("AGENTVAULT_MQTT_TIMEOUT_SECONDS")?),
            max_response_bytes: 2 * 1024 * 1024,
        })?;
        let payer = PayerConfig::new(
            required_env("QUANTSTREAM_SCENARIO_ID")?,
            required_env("QUANTSTREAM_WALLET_ID")?,
            "x402",
            asset,
            required_env("PAYMENT_AMOUNT")?,
            required_env("PAYMENT_RECIPIENT")?,
        )?
        .with_approval_wait(
            Duration::from_secs(positive_env("AGENTVAULT_APPROVAL_TIMEOUT_SECONDS")?),
            Duration::from_secs(1),
        )
        .with_delivery_retries(1);
        let http = AgentVault::new(mqtt).payer(payer)?.standard().client()?;
        let payment_intents = Arc::new(Mutex::new(HashMap::new()));
        let transport = StreamableHttpClientTransport::with_client(
            AgentVaultMCPClient {
                http,
                payment_intents: Arc::clone(&payment_intents),
            },
            StreamableHttpClientTransportConfig::with_uri(required_env("MARKETPULSE_URL")?),
        );
        let client = Arc::new(().serve(transport).await?);
        let tools = client.list_tools(None).await?;
        for required in [YESTERDAY_TOOL, REALTIME_TOOL] {
            if !tools.tools.iter().any(|tool| tool.name.as_ref() == required) {
                return Err(format!("MarketPulse did not expose required MCP tool {required}").into());
            }
        }
        Ok(Self {
            client,
            payment_intents,
        })
    }

    /// Calls the free previous-day news tool for one ingestion run.
    ///
    /// # Errors
    ///
    /// Returns an error when the MCP request, response validation, or transport fails.
    pub async fn yesterday_news(&self, run_id: &str) -> Result<ToolResult, DynError> {
        self.call(YESTERDAY_TOOL, run_id, &format!("{run_id}.yesterday"), false)
            .await
    }

    /// Calls the paid real-time news tool for one ingestion run.
    ///
    /// # Errors
    ///
    /// Returns an error when payment, MCP transport, or response validation fails.
    pub async fn realtime_news(&self, run_id: &str, fail_after_settlement_once: bool) -> Result<ToolResult, DynError> {
        self.call(
            REALTIME_TOOL,
            run_id,
            &format!("{run_id}.realtime"),
            fail_after_settlement_once,
        )
        .await
    }

    async fn call(
        &self,
        tool: &'static str,
        run_id: &str,
        execution_key: &str,
        fail_after_settlement_once: bool,
    ) -> Result<ToolResult, DynError> {
        let paid = tool == REALTIME_TOOL;
        let mut meta = RequestMetaObject::new();
        meta.insert(
            "agentvault".to_owned(),
            json!({
                "runId": run_id,
                "executionKey": execution_key,
                "paid": paid,
                "failAfterSettlement": fail_after_settlement_once,
            }),
        );
        let mut params = CallToolRequestParams::new(tool).with_arguments(JsonObject::new());
        params.meta = Some(meta);
        let result = self.client.call_tool(params).await?;
        if result.is_error.unwrap_or(false) {
            return Err(format!("MarketPulse MCP tool {tool} returned an error").into());
        }
        let structured = result
            .structured_content
            .ok_or("MarketPulse MCP response omitted structuredContent")?;
        // Persist the MCP business payload, not a language-specific JSON-RPC
        // envelope. All five ingestors use this same structuredContent shape.
        let body = serde_json::to_vec(&structured)?;
        let payment_intent_id = self.payment_intents.lock().await.remove(execution_key);
        if paid && payment_intent_id.is_none() {
            return Err("paid response omitted AgentVault payment metadata".into());
        }
        Ok(ToolResult {
            body,
            payment_intent_id,
        })
    }
}

#[derive(Clone)]
struct AgentVaultMCPClient {
    http: ClientWithMiddleware,
    payment_intents: Arc<Mutex<HashMap<String, String>>>,
}

#[derive(Debug, Error)]
enum MCPHTTPError {
    #[error(transparent)]
    AgentVault(#[from] agentvault_sdk::Error),
    #[error(transparent)]
    Middleware(#[from] reqwest_middleware::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
}

impl StreamableHttpClient for AgentVaultMCPClient {
    type Error = MCPHTTPError;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        let message_value =
            serde_json::to_value(&message).map_err(|error| StreamableHttpError::Client(error.into()))?;
        let operation = MCPOperation::from_message(&message_value);
        let mut request = self
            .http
            .post(uri.as_ref())
            .header("content-type", "application/json")
            .header("accept", "application/json");
        if let Some(session) = session_id {
            request = request.header("mcp-session-id", session.as_ref());
        }
        if let Some(token) = auth_header {
            request = request.bearer_auth(token);
        }
        for (name, value) in custom_headers {
            request = request.header(name, value);
        }
        if let Some(operation) = operation.as_ref() {
            request = request
                .header("x-run-id", &operation.run_id)
                .header("mcp-execution-key", &operation.execution_key);
            if operation.fail_after_settlement {
                request = request.header("x-e2e-fail-after-settlement", "once");
            }
            if operation.paid {
                let context = PayerRequestContext::for_operation(&operation.run_id, "marketpulse-realtime")
                    .map_err(|error| StreamableHttpError::Client(error.into()))?;
                request = request.with_payment(context);
            }
        }
        let request_body = serde_json::to_vec(&message).map_err(|error| StreamableHttpError::Client(error.into()))?;
        let mut response = request
            .body(request_body)
            .send()
            .await
            .map_err(|error| StreamableHttpError::Client(error.into()))?;
        if let (Some(operation), Some(payment)) = (
            operation.as_ref().filter(|value| value.paid),
            response.extensions().get::<AuthorizedPayment>(),
        ) {
            self.payment_intents
                .lock()
                .await
                .insert(operation.execution_key.clone(), payment.intent.id.clone());
        }
        let status = response.status();
        if matches!(status.as_u16(), 202 | 204) {
            return Ok(StreamableHttpPostResponse::Accepted);
        }
        let session = response
            .headers()
            .get("mcp-session-id")
            .and_then(|value| value.to_str().ok())
            .map(ToOwned::to_owned);
        let body = read_limited(&mut response).await.map_err(StreamableHttpError::Client)?;
        if !status.is_success() {
            return Err(StreamableHttpError::UnexpectedServerResponse(Cow::Owned(format!(
                "HTTP {status}: {}",
                String::from_utf8_lossy(&body)
            ))));
        }
        let response = serde_json::from_slice::<ServerJsonRpcMessage>(&body)
            .map_err(|error| StreamableHttpError::Client(error.into()))?;
        Ok(StreamableHttpPostResponse::Json(response, session))
    }

    async fn delete_session(
        &self,
        _uri: Arc<str>,
        _session_id: Arc<str>,
        _auth_header: Option<String>,
        _custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        Ok(())
    }

    async fn get_stream(
        &self,
        _uri: Arc<str>,
        _session_id: Option<Arc<str>>,
        _last_event_id: Option<String>,
        _auth_header: Option<String>,
        _custom_headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxStream<'static, Result<Sse, SseError>>, StreamableHttpError<Self::Error>> {
        Err(StreamableHttpError::ServerDoesNotSupportSse)
    }
}

struct MCPOperation {
    run_id: String,
    execution_key: String,
    paid: bool,
    fail_after_settlement: bool,
}

impl MCPOperation {
    fn from_message(message: &Value) -> Option<Self> {
        if message.get("method")?.as_str()? != "tools/call" {
            return None;
        }
        let agentvault = message.get("params")?.get("_meta")?.get("agentvault")?;
        Some(Self {
            run_id: agentvault.get("runId")?.as_str()?.to_owned(),
            execution_key: agentvault.get("executionKey")?.as_str()?.to_owned(),
            paid: agentvault.get("paid")?.as_bool()?,
            fail_after_settlement: agentvault
                .get("failAfterSettlement")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        })
    }
}

async fn read_limited(response: &mut reqwest::Response) -> Result<Vec<u8>, MCPHTTPError> {
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if body
            .len()
            .checked_add(chunk.len())
            .is_none_or(|length| length > MAX_MCP_RESPONSE_BYTES)
        {
            return Err(MCPHTTPError::Json(serde_json::Error::io(std::io::Error::other(
                "MarketPulse response exceeds configured limit",
            ))));
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn required_env(name: &str) -> Result<String, DynError> {
    let value = env::var(name).map_err(|_| format!("{name} is required"))?;
    if value.trim().is_empty() {
        return Err(format!("{name} must not be blank").into());
    }
    Ok(value)
}

fn positive_env(name: &str) -> Result<u64, DynError> {
    let value = required_env(name)?.parse()?;
    if value == 0 {
        return Err(format!("{name} must be positive").into());
    }
    Ok(value)
}

#[cfg(test)]
#[path = "../../tests/agent/mcp_test.rs"]
mod tests;
