use std::{
    convert::Infallible,
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};

use axum::{
    body::{to_bytes, Body},
    http::{header, request::Parts, HeaderName, HeaderValue, Method, Request, Response, StatusCode},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tower::{Layer, Service};

use crate::{Error, PayeeMiddleware, PayeeOperation, ProtectionOutcome, SettledIncomingPayment, VaultTransport};

const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
const RUN_ID_HEADER: &str = "x-run-id";
const PAYMENT_SIGNATURE_HEADER: &str = "payment-signature";

/// Exact route binding for the Axum payee adapter. Pricing and recipient data
/// remain in the server-owned Offer; request metadata cannot override them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AxumPayeeRoute {
    pub method: Method,
    pub path: String,
    pub scenario_id: String,
    pub offer_id: String,
    pub resource: String,
    pub origin: String,
    pub protocol: String,
    /// Optional MCP tool name. When set, other JSON-RPC operations on the
    /// same HTTP endpoint pass through without starting a payment.
    pub mcp_tool: Option<String>,
}

impl AxumPayeeRoute {
    #[must_use]
    pub fn mcp_tool(
        path: impl Into<String>,
        tool: impl Into<String>,
        scenario_id: impl Into<String>,
        offer_id: impl Into<String>,
        resource: impl Into<String>,
        origin: impl Into<String>,
    ) -> Self {
        Self {
            method: Method::POST,
            path: path.into(),
            scenario_id: scenario_id.into(),
            offer_id: offer_id.into(),
            resource: resource.into(),
            origin: origin.into(),
            protocol: "x402".to_owned(),
            mcp_tool: Some(tool.into()),
        }
    }
}

/// Tower/Axum layer that turns matching requests into a complete payee flow.
/// Unmatched routes pass through without body buffering or Vault I/O.
pub struct AxumPayeeMiddleware<T> {
    core: PayeeMiddleware<T>,
    routes: Vec<AxumPayeeRoute>,
    max_body_bytes: usize,
}

impl<T> Clone for AxumPayeeMiddleware<T> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            routes: self.routes.clone(),
            max_body_bytes: self.max_body_bytes,
        }
    }
}

impl<T> AxumPayeeMiddleware<T> {
    pub(crate) fn new(core: PayeeMiddleware<T>, routes: Vec<AxumPayeeRoute>) -> Self {
        Self {
            core,
            routes,
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }

    #[must_use]
    pub const fn with_max_body_bytes(mut self, max_body_bytes: usize) -> Self {
        self.max_body_bytes = max_body_bytes;
        self
    }
}

impl<S, T> Layer<S> for AxumPayeeMiddleware<T>
where
    T: Clone,
{
    type Service = AxumPayeeService<S, T>;

    fn layer(&self, inner: S) -> Self::Service {
        AxumPayeeService {
            inner,
            core: self.core.clone(),
            routes: self.routes.clone(),
            max_body_bytes: self.max_body_bytes,
        }
    }
}

#[derive(Clone)]
pub struct AxumPayeeService<S, T> {
    inner: S,
    core: PayeeMiddleware<T>,
    routes: Vec<AxumPayeeRoute>,
    max_body_bytes: usize,
}

impl<S, T> Service<Request<Body>> for AxumPayeeService<S, T>
where
    S: Service<Request<Body>, Response = Response<Body>, Error = Infallible> + Clone + Send + 'static,
    S::Future: Send + 'static,
    T: VaultTransport + Clone + 'static,
{
    type Response = Response<Body>;
    type Error = Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context)
    }

    fn call(&mut self, request: Request<Body>) -> Self::Future {
        let route = self
            .routes
            .iter()
            .find(|route| route.method == request.method() && route.path == request.uri().path())
            .cloned();
        let mut inner = self.inner.clone();
        let core = self.core.clone();
        let max_body_bytes = self.max_body_bytes;
        Box::pin(async move {
            let Some(route) = route else {
                return inner.call(request).await;
            };
            handle_protected(&mut inner, &core, request, route, max_body_bytes).await
        })
    }
}

async fn handle_protected<S, T>(
    inner: &mut S,
    core: &PayeeMiddleware<T>,
    request: Request<Body>,
    route: AxumPayeeRoute,
    max_body_bytes: usize,
) -> Result<Response<Body>, Infallible>
where
    S: Service<Request<Body>, Response = Response<Body>, Error = Infallible>,
    T: VaultTransport,
{
    if max_body_bytes == 0 {
        return Ok(error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "PAYEE_CONFIG_INVALID",
            "payee body limit is zero",
        ));
    }
    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, max_body_bytes).await else {
        return Ok(error_response(
            StatusCode::PAYLOAD_TOO_LARGE,
            "REQUEST_TOO_LARGE",
            "protected request body exceeds configured limit",
        ));
    };
    if route
        .mcp_tool
        .as_deref()
        .is_some_and(|tool| !is_mcp_tool_call(&body, tool))
    {
        return inner.call(Request::from_parts(parts, Body::from(body))).await;
    }
    let operation = match payment_operation(&route, &parts, &body, max_body_bytes) {
        Ok(operation) => operation,
        Err(error) => return Ok(error_response(error.status, error.code, &error.message)),
    };
    let run_id = operation.run_id.clone();
    match core.protect(&operation).await {
        Ok(ProtectionOutcome::PaymentRequired { challenge }) => Ok(challenge_response(&challenge)),
        Ok(ProtectionOutcome::Authorized { payment }) => {
            let settlement = *payment;
            let payment_response = match encode_payment_response(&settlement) {
                Ok(value) => value,
                Err(error) => {
                    return Ok(error_response(
                        StatusCode::BAD_GATEWAY,
                        "SETTLEMENT_EVIDENCE_INVALID",
                        &error,
                    ))
                }
            };
            let intent_id = settlement.intent.id.clone();
            let mut request = Request::from_parts(parts, Body::from(body));
            request.extensions_mut().insert(settlement);
            let mut response = inner.call(request).await?;
            if let Ok(value) = HeaderValue::from_str(&payment_response) {
                response
                    .headers_mut()
                    .insert(HeaderName::from_static("payment-response"), value);
            }
            tracing::info!(
                event.name = "agentvault.sdk.payee.resource_released",
                run.id = %run_id,
                payment.intent_id = %intent_id
            );
            Ok(response)
        }
        Err(error) => Ok(sdk_error_response(error)),
    }
}

struct OperationError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

fn payment_operation(
    route: &AxumPayeeRoute,
    parts: &Parts,
    body: &[u8],
    max_body_bytes: usize,
) -> std::result::Result<PayeeOperation, OperationError> {
    let run_id = text_header(&parts.headers, RUN_ID_HEADER).ok_or_else(|| OperationError {
        status: StatusCode::BAD_REQUEST,
        code: "PAYMENT_CONTEXT_REQUIRED",
        message: "x-run-id is required".to_owned(),
    })?;
    let credential_payload = text_header(&parts.headers, PAYMENT_SIGNATURE_HEADER)
        .map(|value| {
            decode_x402_payload(value, max_body_bytes).map_err(|message| OperationError {
                status: StatusCode::BAD_REQUEST,
                code: "PAYMENT_INVALID",
                message,
            })
        })
        .transpose()?;
    Ok(PayeeOperation {
        scenario_id: route.scenario_id.clone(),
        run_id: run_id.to_owned(),
        offer_id: route.offer_id.clone(),
        resource: route.resource.clone(),
        origin: route.origin.clone(),
        protocol: route.protocol.clone(),
        request_hash: request_hash(&parts.method, parts.uri.path(), body),
        credential_payload,
    })
}

fn is_mcp_tool_call(body: &[u8], expected: &str) -> bool {
    serde_json::from_slice::<Value>(body)
        .ok()
        .and_then(|value| {
            (value.get("method")?.as_str()? == "tools/call")
                .then(|| value.get("params")?.get("name")?.as_str())
                .flatten()
                .map(str::to_owned)
        })
        .is_some_and(|tool| tool == expected)
}

fn text_header<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .filter(|value| !value.trim().is_empty())
}

fn decode_x402_payload(value: &str, limit: usize) -> Result<Value, String> {
    if value.len() > limit.saturating_mul(4).div_ceil(3).saturating_add(4) {
        return Err("PAYMENT-SIGNATURE exceeds configured limit".to_owned());
    }
    let decoded = STANDARD
        .decode(value)
        .map_err(|_| "PAYMENT-SIGNATURE is not canonical base64".to_owned())?;
    if decoded.len() > limit || STANDARD.encode(&decoded) != value {
        return Err("PAYMENT-SIGNATURE is non-canonical or oversized".to_owned());
    }
    let payload: Value =
        serde_json::from_slice(&decoded).map_err(|_| "PAYMENT-SIGNATURE is not valid x402 JSON".to_owned())?;
    if !payload.is_object() {
        return Err("PAYMENT-SIGNATURE must be an x402 JSON object".to_owned());
    }
    Ok(payload)
}

fn request_hash(method: &Method, path: &str, body: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut hasher = Sha256::new();
    hasher.update(b"agentvault.payee.http.v1\0");
    hasher.update(method.as_str().as_bytes());
    hasher.update(b"\0");
    hasher.update(path.as_bytes());
    hasher.update(b"\0");
    hasher.update(body);
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push(char::from(HEX[usize::from(byte >> 4)]));
        encoded.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    encoded
}

fn challenge_response(challenge: &crate::PaymentChallenge) -> Response<Body> {
    let Some(required) = challenge.payload.get("payment_required") else {
        return error_response(
            StatusCode::BAD_GATEWAY,
            "CHALLENGE_INVALID",
            "AgentVault returned no protocol wire challenge",
        );
    };
    let Ok(body) = serde_json::to_vec(required) else {
        return error_response(
            StatusCode::BAD_GATEWAY,
            "CHALLENGE_INVALID",
            "AgentVault challenge cannot be encoded",
        );
    };
    let mut response = Response::new(Body::from(body.clone()));
    *response.status_mut() = StatusCode::PAYMENT_REQUIRED;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    if let Ok(protocol) = HeaderValue::from_str(&challenge.protocol) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("payment-protocol"), protocol);
    }
    if let Ok(expires_at) = HeaderValue::from_str(&challenge.expires_at.to_string()) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("payment-expires-at"), expires_at);
    }
    if let Ok(required) = HeaderValue::from_str(&STANDARD.encode(&body)) {
        response
            .headers_mut()
            .insert(HeaderName::from_static("payment-required"), required);
    }
    response
}

fn encode_payment_response(payment: &SettledIncomingPayment) -> Result<String, String> {
    let transaction = payment
        .receipt
        .tx_hash
        .as_deref()
        .ok_or_else(|| "confirmed x402 receipt has no transaction".to_owned())?;
    let payer = payment
        .receipt
        .payer
        .as_deref()
        .ok_or_else(|| "confirmed x402 receipt has no trusted payer".to_owned())?;
    let wire = json!({
        "success": true,
        "payer": payer,
        "transaction": transaction,
        "network": payment.receipt.asset.network,
    });
    serde_json::to_vec(&wire)
        .map(|body| STANDARD.encode(body))
        .map_err(|error| error.to_string())
}

fn sdk_error_response(error: Error) -> Response<Body> {
    match error {
        Error::Conflict { code, message } => error_response(StatusCode::CONFLICT, &code, &message),
        Error::Api { status, code, message } => {
            let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
            error_response(status, &code, &message)
        }
        Error::InvalidArgument(message) | Error::UnsafeRetry(message) => {
            error_response(StatusCode::BAD_REQUEST, "PAYMENT_INVALID", &message)
        }
        other => error_response(StatusCode::BAD_GATEWAY, "AGENTVAULT_UNAVAILABLE", &other.to_string()),
    }
}

fn error_response(status: StatusCode, code: &str, message: &str) -> Response<Body> {
    let body = serde_json::to_vec(&json!({"error": {"code": code, "message": message}}))
        .unwrap_or_else(|_| b"{\"error\":{\"code\":\"INTERNAL_ERROR\"}}".to_vec());
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

#[cfg(test)]
#[path = "../../tests/payee/axum_test.rs"]
mod tests;
