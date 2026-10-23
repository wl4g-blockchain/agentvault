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
use serde_json::{json, Value};
use sha2::{Digest as _, Sha256};
use tower::{Layer, Service};

use crate::payee_core::PayeeProtectionError;
use crate::{Error, PayeeMiddleware, PayeeOperation, ProtectionOutcome, VaultTransport};

const DEFAULT_MAX_BODY_BYTES: usize = 1024 * 1024;
const RUN_ID_HEADER: &str = "x-run-id";

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
        Self::mcp_tool_with_protocol(path, tool, scenario_id, offer_id, resource, origin, "x402")
    }

    #[must_use]
    pub fn mcp_tool_with_protocol(
        path: impl Into<String>,
        tool: impl Into<String>,
        scenario_id: impl Into<String>,
        offer_id: impl Into<String>,
        resource: impl Into<String>,
        origin: impl Into<String>,
        protocol: impl Into<String>,
    ) -> Self {
        Self {
            method: Method::POST,
            path: path.into(),
            scenario_id: scenario_id.into(),
            offer_id: offer_id.into(),
            resource: resource.into(),
            origin: origin.into(),
            protocol: protocol.into(),
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
    let mcp_call = if let Some(tool) = route.mcp_tool.as_deref() {
        match McpCall::parse(
            &body,
            tool,
            &core.mcp_request_metadata_keys(),
            &core.mcp_root_request_metadata_keys(),
        ) {
            Ok(Some(call)) => Some(call),
            Ok(None) => return inner.call(Request::from_parts(parts, Body::from(body))).await,
            Err(error) => return Ok(error_response(error.status, error.code, &error.message)),
        }
    } else {
        None
    };
    let operation = match payment_operation(core, &route, &parts, &body, mcp_call.as_ref()) {
        Ok(operation) => operation,
        Err(error) => return Ok(error_response(error.status, error.code, &error.message)),
    };
    let run_id = operation.run_id.clone();
    match core.protect_with_challenge(&operation).await {
        Ok((ProtectionOutcome::PaymentRequired { challenge }, _)) => {
            if let Some(call) = &mcp_call {
                Ok(AxumProtocolResponse::mcp_challenge(core, &challenge, &call.id))
            } else {
                Ok(AxumProtocolResponse::challenge(core, &challenge))
            }
        }
        Ok((ProtectionOutcome::Authorized { payment }, challenge)) => {
            release_authorized(
                inner,
                core,
                AuthorizedRelease {
                    parts,
                    body: &body,
                    mcp_call: mcp_call.as_ref(),
                    challenge: &challenge,
                    settlement: *payment,
                    max_body_bytes,
                    run_id: &run_id,
                },
            )
            .await
        }
        Err(PayeeProtectionError::Rejected { challenge, message }) => {
            if let Some(call) = &mcp_call {
                Ok(AxumProtocolResponse::mcp_rejection(
                    core,
                    challenge.as_ref(),
                    &call.id,
                    &message,
                ))
            } else {
                Ok(AxumProtocolResponse::challenge(core, challenge.as_ref()))
            }
        }
        Err(PayeeProtectionError::Sdk(error)) => Ok(sdk_error_response(error)),
    }
}

struct AuthorizedRelease<'a> {
    parts: Parts,
    body: &'a [u8],
    mcp_call: Option<&'a McpCall>,
    challenge: &'a crate::PaymentChallenge,
    settlement: crate::SettledIncomingPayment,
    max_body_bytes: usize,
    run_id: &'a str,
}

async fn release_authorized<S, T>(
    inner: &mut S,
    core: &PayeeMiddleware<T>,
    release: AuthorizedRelease<'_>,
) -> Result<Response<Body>, Infallible>
where
    S: Service<Request<Body>, Response = Response<Body>, Error = Infallible>,
{
    let AuthorizedRelease {
        parts,
        body,
        mcp_call,
        challenge,
        settlement,
        max_body_bytes,
        run_id,
    } = release;
    let intent_id = settlement.intent.id.clone();
    let replay_body = mcp_call.map_or_else(|| body.to_vec(), |call| call.canonical_body.clone());
    let mut request = Request::from_parts(parts, Body::from(replay_body));
    request.extensions_mut().insert(settlement.clone());
    let mut response = inner.call(request).await?;
    if core
        .merchant_evidence_header_names()
        .iter()
        .any(|name| response.headers().contains_key(*name))
    {
        return Ok(error_response(
            StatusCode::BAD_GATEWAY,
            "SETTLEMENT_EVIDENCE_INVALID",
            "paid handler attempted to supply settlement evidence",
        ));
    }
    if let Some(call) = mcp_call {
        response =
            AxumProtocolResponse::mcp_receipt(core, challenge, &settlement, &call.id, response, max_body_bytes).await;
    } else {
        let receipt = match core.receipt_response(&settlement) {
            Ok(value) => value,
            Err(error) => {
                return Ok(error_response(
                    StatusCode::BAD_GATEWAY,
                    "SETTLEMENT_EVIDENCE_INVALID",
                    &error.to_string(),
                ))
            }
        };
        if let Err(error) = AxumProtocolResponse::apply_headers(response.headers_mut(), &receipt.headers) {
            return Ok(error_response(
                StatusCode::BAD_GATEWAY,
                "SETTLEMENT_EVIDENCE_INVALID",
                &error,
            ));
        }
    }
    if response.status().is_success() {
        tracing::info!(
            event.name = "agentvault.sdk.payee.resource_released",
            run.id = %run_id,
            payment.intent_id = %intent_id
        );
    }
    Ok(response)
}

#[derive(Debug)]
struct OperationError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

struct McpCall {
    id: Value,
    meta: Value,
    canonical_body: Vec<u8>,
}

impl McpCall {
    fn parse(
        body: &[u8],
        expected_tool: &str,
        payment_metadata_keys: &[&str],
        root_payment_metadata_keys: &[&str],
    ) -> std::result::Result<Option<Self>, OperationError> {
        let Ok(mut message) = serde_json::from_slice::<Value>(body) else {
            return Ok(None);
        };
        if message.get("jsonrpc").and_then(Value::as_str) != Some("2.0")
            || message.get("method").and_then(Value::as_str) != Some("tools/call")
            || message.pointer("/params/name").and_then(Value::as_str) != Some(expected_tool)
        {
            return Ok(None);
        }
        let id = message
            .get("id")
            .cloned()
            .filter(|value| value.is_string() || value.is_number())
            .ok_or_else(|| OperationError {
                status: StatusCode::BAD_REQUEST,
                code: "MCP_REQUEST_INVALID",
                message: "paid MCP tools/call requires a string or numeric JSON-RPC id".to_owned(),
            })?;
        let params = message
            .get("params")
            .and_then(Value::as_object)
            .ok_or_else(|| OperationError {
                status: StatusCode::BAD_REQUEST,
                code: "MCP_REQUEST_INVALID",
                message: "MCP tools/call params must be an object".to_owned(),
            })?;
        let nested_meta = params.get("_meta").cloned().unwrap_or_else(|| json!({}));
        let mut meta = nested_meta.as_object().cloned().ok_or_else(|| OperationError {
            status: StatusCode::BAD_REQUEST,
            code: "PAYMENT_INVALID",
            message: "MCP params._meta must be an object".to_owned(),
        })?;
        let root_meta = message.get("_meta").cloned().unwrap_or_else(|| json!({}));
        let root_meta = root_meta.as_object().ok_or_else(|| OperationError {
            status: StatusCode::BAD_REQUEST,
            code: "PAYMENT_INVALID",
            message: "JSON-RPC root _meta must be an object".to_owned(),
        })?;
        for key in root_payment_metadata_keys {
            if let Some(value) = root_meta.get(*key) {
                if meta.contains_key(*key) {
                    return Err(OperationError {
                        status: StatusCode::BAD_REQUEST,
                        code: "PAYMENT_INVALID",
                        message: "duplicate MCP payment credential metadata".to_owned(),
                    });
                }
                meta.insert((*key).to_owned(), value.clone());
            }
        }
        let params = message
            .get_mut("params")
            .and_then(Value::as_object_mut)
            .expect("MCP params were validated as an object");
        if let Some(canonical_meta) = params.get_mut("_meta").and_then(Value::as_object_mut) {
            for key in payment_metadata_keys {
                canonical_meta.remove(*key);
            }
            if canonical_meta.is_empty() {
                params.remove("_meta");
            }
        }
        let remove_root_meta = if let Some(canonical_meta) = message.get_mut("_meta").and_then(Value::as_object_mut) {
            for key in root_payment_metadata_keys {
                canonical_meta.remove(*key);
            }
            canonical_meta.is_empty()
        } else {
            false
        };
        if remove_root_meta {
            message
                .as_object_mut()
                .expect("MCP message is an object")
                .remove("_meta");
        }
        Self::sort_object_keys(&mut message);
        let canonical_body = serde_json::to_vec(&message).map_err(|error| OperationError {
            status: StatusCode::BAD_REQUEST,
            code: "MCP_REQUEST_INVALID",
            message: format!("MCP request cannot be canonicalized: {error}"),
        })?;
        Ok(Some(Self {
            id,
            meta: Value::Object(meta),
            canonical_body,
        }))
    }

    fn sort_object_keys(value: &mut Value) {
        match value {
            Value::Array(values) => values.iter_mut().for_each(Self::sort_object_keys),
            Value::Object(object) => {
                let mut fields = std::mem::take(object).into_iter().collect::<Vec<_>>();
                fields.sort_unstable_by(|left, right| left.0.cmp(&right.0));
                for (name, mut field) in fields {
                    Self::sort_object_keys(&mut field);
                    object.insert(name, field);
                }
            }
            _ => {}
        }
    }
}

fn payment_operation<T>(
    core: &PayeeMiddleware<T>,
    route: &AxumPayeeRoute,
    parts: &Parts,
    body: &[u8],
    mcp_call: Option<&McpCall>,
) -> std::result::Result<PayeeOperation, OperationError> {
    let run_id = text_header(&parts.headers, RUN_ID_HEADER).ok_or_else(|| OperationError {
        status: StatusCode::BAD_REQUEST,
        code: "PAYMENT_CONTEXT_REQUIRED",
        message: "x-run-id is required".to_owned(),
    })?;
    let headers = payment_headers(&parts.headers, &core.credential_header_names())?;
    let credential_payload = if let Some(call) = mcp_call {
        match core.presented_credential(&route.protocol, &headers) {
            Ok(Some(_)) => {
                return Err(OperationError {
                    status: StatusCode::BAD_REQUEST,
                    code: "PAYMENT_INVALID",
                    message: "MCP payment credentials must use protocol metadata, not HTTP headers".to_owned(),
                })
            }
            Ok(None) => {}
            Err(error) => {
                return Err(OperationError {
                    status: StatusCode::BAD_REQUEST,
                    code: "PAYMENT_INVALID",
                    message: error.to_string(),
                })
            }
        }
        core.presented_mcp_credential(&route.protocol, &call.meta)
            .map_err(|error| OperationError {
                status: StatusCode::BAD_REQUEST,
                code: "PAYMENT_INVALID",
                message: error.to_string(),
            })?
    } else {
        core.presented_credential(&route.protocol, &headers)
            .map_err(|error| OperationError {
                status: StatusCode::BAD_REQUEST,
                code: "PAYMENT_INVALID",
                message: error.to_string(),
            })?
    };
    let hash_body = mcp_call.map_or(body, |call| call.canonical_body.as_slice());
    Ok(PayeeOperation {
        scenario_id: route.scenario_id.clone(),
        run_id: run_id.to_owned(),
        offer_id: route.offer_id.clone(),
        resource: route.resource.clone(),
        origin: route.origin.clone(),
        protocol: route.protocol.clone(),
        request_hash: request_hash(&parts.method, parts.uri.path(), hash_body),
        credential_payload,
    })
}

fn payment_headers(
    source: &axum::http::HeaderMap,
    names: &[&str],
) -> std::result::Result<std::collections::BTreeMap<String, String>, OperationError> {
    let mut headers = std::collections::BTreeMap::new();
    for name in names {
        let mut values = source.get_all(*name).iter();
        let Some(value) = values.next() else {
            continue;
        };
        if values.next().is_some() {
            return Err(OperationError {
                status: StatusCode::BAD_REQUEST,
                code: "PAYMENT_INVALID",
                message: format!("duplicate payment request header is not allowed: {name}"),
            });
        }
        let value = value.to_str().map_err(|_| OperationError {
            status: StatusCode::BAD_REQUEST,
            code: "PAYMENT_INVALID",
            message: "payment headers must contain text values".to_owned(),
        })?;
        headers.insert((*name).to_owned(), value.to_owned());
    }
    Ok(headers)
}

#[cfg(test)]
fn is_mcp_tool_call(body: &[u8], expected: &str) -> bool {
    McpCall::parse(body, expected, &[], &[]).is_ok_and(|call| call.is_some())
}

fn text_header<'a>(headers: &'a axum::http::HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get(name)?
        .to_str()
        .ok()
        .filter(|value| !value.trim().is_empty())
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

struct AxumProtocolResponse;

impl AxumProtocolResponse {
    fn challenge<T>(core: &PayeeMiddleware<T>, challenge: &crate::PaymentChallenge) -> Response<Body> {
        let wire = match core.challenge_response(challenge) {
            Ok(wire) => wire,
            Err(error) => return error_response(StatusCode::BAD_GATEWAY, "CHALLENGE_INVALID", &error.to_string()),
        };
        let mut response = Response::new(Body::from(wire.body));
        *response.status_mut() = StatusCode::PAYMENT_REQUIRED;
        if let Err(error) = Self::apply_headers(response.headers_mut(), &wire.headers) {
            return error_response(StatusCode::BAD_GATEWAY, "CHALLENGE_INVALID", &error);
        }
        if let Some(content_type) = wire.content_type {
            let Ok(content_type) = HeaderValue::from_str(&content_type) else {
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    "CHALLENGE_INVALID",
                    "protocol challenge content type is invalid",
                );
            };
            response.headers_mut().insert(header::CONTENT_TYPE, content_type);
        }
        if Self::apply_challenge_metadata(response.headers_mut(), challenge).is_err() {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "CHALLENGE_INVALID",
                "protocol challenge metadata is invalid",
            );
        }
        response
    }

    fn mcp_challenge<T>(core: &PayeeMiddleware<T>, challenge: &crate::PaymentChallenge, id: &Value) -> Response<Body> {
        let wire = match core.mcp_challenge_response(challenge) {
            Ok(wire) => wire,
            Err(error) => return error_response(StatusCode::BAD_GATEWAY, "CHALLENGE_INVALID", &error.to_string()),
        };
        Self::mcp_payment_response(wire, id, challenge)
    }

    fn mcp_rejection<T>(
        core: &PayeeMiddleware<T>,
        challenge: &crate::PaymentChallenge,
        id: &Value,
        message: &str,
    ) -> Response<Body> {
        let wire = match core.mcp_rejection_response(challenge, message) {
            Ok(wire) => wire,
            Err(error) => return error_response(StatusCode::BAD_GATEWAY, "CHALLENGE_INVALID", &error.to_string()),
        };
        Self::mcp_payment_response(wire, id, challenge)
    }

    fn mcp_payment_response(
        wire: crate::ProtocolMcpPaymentRequired,
        id: &Value,
        challenge: &crate::PaymentChallenge,
    ) -> Response<Body> {
        let wire = match wire {
            crate::ProtocolMcpPaymentRequired::Result(result) => {
                json!({"jsonrpc": "2.0", "id": id, "result": result})
            }
            crate::ProtocolMcpPaymentRequired::Error(error) => {
                json!({"jsonrpc": "2.0", "id": id, "error": error})
            }
        };
        let Ok(body) = serde_json::to_vec(&wire) else {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "CHALLENGE_INVALID",
                "MCP challenge cannot be encoded",
            );
        };
        let mut response = Response::new(Body::from(body));
        response
            .headers_mut()
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        if Self::apply_challenge_metadata(response.headers_mut(), challenge).is_err() {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "CHALLENGE_INVALID",
                "protocol challenge metadata is invalid",
            );
        }
        response
    }

    fn apply_challenge_metadata(
        headers: &mut axum::http::HeaderMap,
        challenge: &crate::PaymentChallenge,
    ) -> std::result::Result<(), ()> {
        for (name, value) in [
            ("payment-protocol", challenge.protocol.clone()),
            ("payment-expires-at", challenge.expires_at.to_string()),
        ] {
            let value = HeaderValue::from_str(&value).map_err(|_| ())?;
            headers.insert(HeaderName::from_static(name), value);
        }
        Ok(())
    }

    async fn mcp_receipt<T>(
        core: &PayeeMiddleware<T>,
        challenge: &crate::PaymentChallenge,
        payment: &crate::SettledIncomingPayment,
        id: &Value,
        response: Response<Body>,
        limit: usize,
    ) -> Response<Body> {
        if !response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("application/json"))
        {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "MCP_RESPONSE_INVALID",
                "paid MCP handlers must return a bounded application/json result",
            );
        }
        let (mut parts, body) = response.into_parts();
        let Ok(body) = to_bytes(body, limit).await else {
            return error_response(
                StatusCode::BAD_GATEWAY,
                "MCP_RESPONSE_INVALID",
                "paid MCP response exceeds configured limit",
            );
        };
        let message = match Self::decode_mcp_handler_response(core, id, &body) {
            Ok(value) => value,
            Err(message) => return error_response(StatusCode::BAD_GATEWAY, "MCP_RESPONSE_INVALID", message),
        };
        if !parts.status.is_success() {
            return Response::from_parts(parts, Body::from(body));
        }
        let metadata = match core.mcp_receipt(challenge, payment) {
            Ok(value) => value,
            Err(error) => {
                return error_response(
                    StatusCode::BAD_GATEWAY,
                    "SETTLEMENT_EVIDENCE_INVALID",
                    &error.to_string(),
                )
            }
        };
        let body = match Self::encode_mcp_receipt(message, metadata, limit) {
            Ok(value) => value,
            Err(message) => return error_response(StatusCode::BAD_GATEWAY, "MCP_RESPONSE_INVALID", message),
        };
        parts.headers.remove(header::CONTENT_LENGTH);
        parts
            .headers
            .insert(header::CONTENT_TYPE, HeaderValue::from_static("application/json"));
        Response::from_parts(parts, Body::from(body))
    }

    fn decode_mcp_handler_response<T>(
        core: &PayeeMiddleware<T>,
        id: &Value,
        body: &[u8],
    ) -> std::result::Result<Value, &'static str> {
        let message = serde_json::from_slice::<Value>(body).map_err(|_| "paid MCP handler returned invalid JSON")?;
        if message.get("id") != Some(id) {
            return Err("paid MCP response id does not match its request");
        }
        let supplied_evidence = message
            .get("result")
            .and_then(|result| result.get("_meta"))
            .and_then(Value::as_object)
            .is_some_and(|meta| {
                core.mcp_receipt_metadata_keys()
                    .iter()
                    .any(|key| meta.contains_key(*key))
            });
        if supplied_evidence {
            return Err("paid MCP handler attempted to supply settlement metadata");
        }
        Ok(message)
    }

    fn encode_mcp_receipt(
        mut message: Value,
        metadata: crate::ProtocolMcpMetadata,
        limit: usize,
    ) -> std::result::Result<Vec<u8>, &'static str> {
        let result = message
            .get_mut("result")
            .and_then(Value::as_object_mut)
            .ok_or("paid MCP handler must return a JSON-RPC result")?;
        let meta = result
            .entry("_meta")
            .or_insert_with(|| Value::Object(serde_json::Map::new()))
            .as_object_mut()
            .ok_or("paid MCP result._meta must be an object")?;
        if meta.contains_key(metadata.key) {
            return Err("paid MCP handler attempted to supply settlement metadata");
        }
        meta.insert(metadata.key.to_owned(), metadata.value);
        let body = serde_json::to_vec(&message).map_err(|_| "paid MCP response cannot be encoded")?;
        if body.len() > limit {
            return Err("paid MCP response exceeds configured limit");
        }
        Ok(body)
    }

    fn apply_headers(
        target: &mut axum::http::HeaderMap,
        headers: &std::collections::BTreeMap<String, String>,
    ) -> Result<(), String> {
        for (name, value) in headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| "protocol response header name is invalid".to_owned())?;
            let value =
                HeaderValue::from_str(value).map_err(|_| "protocol response header value is invalid".to_owned())?;
            target.insert(name, value);
        }
        Ok(())
    }
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
