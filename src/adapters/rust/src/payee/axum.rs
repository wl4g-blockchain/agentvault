use std::{
    collections::BTreeMap,
    convert::Infallible,
    future::Future,
    pin::Pin,
    sync::Arc,
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
/// remain in the server-owned Price; request metadata cannot override them.
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
    a2a_stores: BTreeMap<String, Arc<dyn crate::A2aTaskStore>>,
    max_body_bytes: usize,
}

impl<T> Clone for AxumPayeeMiddleware<T> {
    fn clone(&self) -> Self {
        Self {
            core: self.core.clone(),
            routes: self.routes.clone(),
            a2a_stores: self.a2a_stores.clone(),
            max_body_bytes: self.max_body_bytes,
        }
    }
}

impl<T> AxumPayeeMiddleware<T> {
    pub(crate) fn new(core: PayeeMiddleware<T>, routes: Vec<AxumPayeeRoute>) -> Self {
        Self {
            core,
            routes,
            a2a_stores: BTreeMap::new(),
            max_body_bytes: DEFAULT_MAX_BODY_BYTES,
        }
    }

    /// Uses the application's task store for the explicitly selected non-streaming A2A route.
    #[must_use]
    pub fn with_a2a_task_store(mut self, path: impl Into<String>, store: Arc<dyn crate::A2aTaskStore>) -> Self {
        self.a2a_stores.insert(path.into(), store);
        self
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
            a2a_stores: self.a2a_stores.clone(),
            max_body_bytes: self.max_body_bytes,
        }
    }
}

#[derive(Clone)]
pub struct AxumPayeeService<S, T> {
    inner: S,
    core: PayeeMiddleware<T>,
    routes: Vec<AxumPayeeRoute>,
    a2a_stores: BTreeMap<String, Arc<dyn crate::A2aTaskStore>>,
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
        let store = self.a2a_stores.get(request.uri().path()).cloned();
        let mut inner = self.inner.clone();
        let core = self.core.clone();
        let max_body_bytes = self.max_body_bytes;
        Box::pin(async move {
            let Some(route) = route else {
                return inner.call(request).await;
            };
            AxumRequest::handle_protected(&mut inner, &core, request, route, max_body_bytes, store).await
        })
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

#[derive(Debug)]
struct OperationError {
    status: StatusCode,
    code: &'static str,
    message: String,
}

#[path = "axum_message.rs"]
mod message;
use message::{AxumProtocolResponse, McpCall};

#[cfg(test)]
#[path = "../../tests/payee/axum_test.rs"]
mod tests;

struct AxumRequest;

impl AxumRequest {
    async fn handle_protected<S, T>(
        inner: &mut S,
        core: &PayeeMiddleware<T>,
        request: Request<Body>,
        route: AxumPayeeRoute,
        max_body_bytes: usize,
        store: Option<Arc<dyn crate::A2aTaskStore>>,
    ) -> Result<Response<Body>, Infallible>
    where
        S: Service<Request<Body>, Response = Response<Body>, Error = Infallible>,
        T: VaultTransport,
    {
        if max_body_bytes == 0 {
            return Ok(AxumRequest::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "PAYEE_CONFIG_INVALID",
                "payee body limit is zero",
            ));
        }
        let (parts, body) = request.into_parts();
        let Ok(body) = to_bytes(body, max_body_bytes).await else {
            return Ok(AxumRequest::error_response(
                StatusCode::PAYLOAD_TOO_LARGE,
                "REQUEST_TOO_LARGE",
                "protected request body exceeds configured limit",
            ));
        };
        let mut mcp_call = if let Some(tool) = route.mcp_tool.as_deref() {
            match McpCall::parse(
                &body,
                tool,
                &core.mcp_request_metadata_keys(),
                &core.mcp_root_request_metadata_keys(),
            ) {
                Ok(Some(call)) => Some(call),
                Ok(None) => return inner.call(Request::from_parts(parts, Body::from(body))).await,
                Err(error) => return Ok(AxumRequest::error_response(error.status, error.code, &error.message)),
            }
        } else {
            None
        };
        let mut operation = match AxumRequest::payment_operation(core, &route, &parts, &body, mcp_call.as_ref()) {
            Ok(operation) => operation,
            Err(error) => return Ok(AxumRequest::error_response(error.status, error.code, &error.message)),
        };
        if let Some(store) = store {
            if route.mcp_tool.is_some() || operation.credential_payload.is_some() {
                return Ok(AxumRequest::sdk_error_response(crate::A2aCall::invalid(
                    "A2A/MCP/HTTP payment carrier conflict",
                )));
            }
            match McpCall::a2a(core, &body, &mut operation, store, &parts).await {
                Ok(Some(call)) => mcp_call = Some(call),
                Ok(None) => return inner.call(Request::from_parts(parts, Body::from(body))).await,
                Err(e) => return Ok(AxumRequest::sdk_error_response(e)),
            }
        }
        let run_id = operation.run_id.clone();
        match core.protect_with_challenge(&operation).await {
            Ok((ProtectionOutcome::PaymentRequired { challenge }, _)) => {
                if let Some(call) = &mcp_call {
                    if let Some(a2a) = &call.a2a {
                        Ok(AxumProtocolResponse::a2a_challenge(core, a2a, &challenge, false).await)
                    } else {
                        Ok(AxumProtocolResponse::mcp_challenge(core, &challenge, &call.id))
                    }
                } else {
                    Ok(AxumProtocolResponse::challenge(core, &challenge))
                }
            }
            Ok((ProtectionOutcome::Authorized { payment }, challenge)) => {
                AxumRequest::release_authorized(
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
                    if let Some(a2a) = &call.a2a {
                        return Ok(AxumProtocolResponse::a2a_challenge(core, a2a, challenge.as_ref(), true).await);
                    }
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
            Err(PayeeProtectionError::Sdk(error)) => Ok(AxumRequest::sdk_error_response(error)),
        }
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
            return Ok(AxumRequest::error_response(
                StatusCode::BAD_GATEWAY,
                "SETTLEMENT_EVIDENCE_INVALID",
                "paid handler attempted to supply settlement evidence",
            ));
        }
        if let Some(call) = mcp_call {
            response = if let Some(a2a) = &call.a2a {
                AxumProtocolResponse::a2a_receipt(core, a2a, challenge, &settlement, response, max_body_bytes).await
            } else {
                AxumProtocolResponse::mcp_receipt(core, challenge, &settlement, &call.id, response, max_body_bytes)
                    .await
            };
        } else {
            let receipt = match core.receipt_response(&settlement) {
                Ok(value) => value,
                Err(error) => {
                    return Ok(AxumRequest::error_response(
                        StatusCode::BAD_GATEWAY,
                        "SETTLEMENT_EVIDENCE_INVALID",
                        &error.to_string(),
                    ))
                }
            };
            if let Err(error) = AxumProtocolResponse::apply_headers(response.headers_mut(), &receipt.headers) {
                return Ok(AxumRequest::error_response(
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

    fn payment_operation<T>(
        core: &PayeeMiddleware<T>,
        route: &AxumPayeeRoute,
        parts: &Parts,
        body: &[u8],
        mcp_call: Option<&McpCall>,
    ) -> std::result::Result<PayeeOperation, OperationError> {
        let run_id = AxumRequest::text_header(&parts.headers, RUN_ID_HEADER).ok_or_else(|| OperationError {
            status: StatusCode::BAD_REQUEST,
            code: "PAYMENT_CONTEXT_REQUIRED",
            message: "x-run-id is required".to_owned(),
        })?;
        let headers = AxumRequest::payment_headers(&parts.headers, &core.credential_header_names())?;
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
            request_hash: AxumRequest::request_hash(&parts.method, parts.uri.path(), hash_body),
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

    fn sdk_error_response(error: Error) -> Response<Body> {
        match error {
            Error::Conflict { code, message } => AxumRequest::error_response(StatusCode::CONFLICT, &code, &message),
            Error::Api { status, code, message } => {
                let status = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_GATEWAY);
                AxumRequest::error_response(status, &code, &message)
            }
            Error::InvalidArgument(message) | Error::UnsafeRetry(message) => {
                AxumRequest::error_response(StatusCode::BAD_REQUEST, "PAYMENT_INVALID", &message)
            }
            other => AxumRequest::error_response(StatusCode::BAD_GATEWAY, "AGENTVAULT_UNAVAILABLE", &other.to_string()),
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
}
