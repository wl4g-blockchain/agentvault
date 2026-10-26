use std::{collections::BTreeMap, time::Duration};

use axum::{
    body::{to_bytes, Body},
    http::{HeaderMap, HeaderName, HeaderValue, Method, Request, Uri},
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use tonic::{Request as GrpcRequest, Response as GrpcResponse, Status};
use tower::ServiceExt as _;
use tracing::Instrument as _;

use crate::{
    apm::{set_span_parent_from_headers, AgentVaultMetrics},
    controller::principal_context::{CallerPrincipal, PrincipalContext, PrincipalResolutionError},
    grpc_wire::{payment_transport_server::PaymentTransport, Envelope},
};

const API_PREFIX: &str = "/api/v1";
const MAX_HEADERS: usize = 64;
const ALLOWED_REQUEST_HEADERS: [&str; 3] = ["accept", "content-type", "x-request-id"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WireRequest {
    method: String,
    path: String,
    #[serde(default)]
    headers: BTreeMap<String, String>,
    body: String,
}

#[derive(Serialize)]
struct WireResponse {
    status: u16,
    headers: BTreeMap<String, String>,
    body: String,
}

#[derive(Clone)]
pub(crate) struct GrpcEnvelopeService {
    pub(crate) router: Router,
    pub(crate) principals: PrincipalContext,
    pub(crate) metrics: AgentVaultMetrics,
    pub(crate) max_message_bytes: usize,
    pub(crate) timeout: Duration,
}

#[tonic::async_trait]
impl PaymentTransport for GrpcEnvelopeService {
    async fn execute(&self, request: GrpcRequest<Envelope>) -> Result<GrpcResponse<Envelope>, Status> {
        let trace_headers = GrpcEnvelopeService::trace_headers(&request)?;
        let span = tracing::info_span!(
            "grpc.server.request",
            otel.kind = "server",
            rpc.system = "grpc",
            rpc.service = "agentvault.transport.v1.PaymentTransport",
            rpc.method = "Execute",
            rpc.grpc.status_code = tracing::field::Empty,
        );
        set_span_parent_from_headers(&span, &trace_headers);
        let result = self
            .execute_inner(request, trace_headers)
            .instrument(span.clone())
            .await;
        let status = result.as_ref().map_or_else(|error| error.code() as i32, |_| 0);
        span.record("rpc.grpc.status_code", status);
        result
    }
}

impl GrpcEnvelopeService {
    async fn execute_inner(
        &self,
        request: GrpcRequest<Envelope>,
        trace_headers: HeaderMap,
    ) -> Result<GrpcResponse<Envelope>, Status> {
        let (trace_id, producer_span_id) = GrpcEnvelopeService::trace_identity(&trace_headers)
            .ok_or_else(|| Status::invalid_argument("valid W3C traceparent metadata is required"))?;
        let principal = self.resolve_principal(&request).await?;
        let encoded = request.into_inner().value;
        if encoded.len() > self.max_message_bytes {
            return Err(Status::resource_exhausted("gRPC payment envelope is too large"));
        }
        let wire: WireRequest =
            serde_json::from_slice(&encoded).map_err(|_| Status::invalid_argument("invalid gRPC payment envelope"))?;
        let method = match wire.method.as_str() {
            "GET" => Method::GET,
            "POST" => Method::POST,
            _ => return Err(Status::invalid_argument("unsupported payment method")),
        };
        let uri = wire
            .path
            .parse::<Uri>()
            .ok()
            .filter(|uri| uri.scheme().is_none() && uri.authority().is_none())
            .ok_or_else(|| Status::invalid_argument("payment path must be a relative canonical URI"))?;
        let path = uri
            .path()
            .strip_prefix(API_PREFIX)
            .filter(|path| path.starts_with('/'))
            .ok_or_else(|| Status::invalid_argument("payment path must use the canonical API prefix"))?;
        let metric_route = GrpcEnvelopeService::canonical_payment_route(&method, path)
            .ok_or_else(|| Status::permission_denied("gRPC transport does not expose this payment route"))?;
        tracing::info!(
            event.name = "agentvault.transport.grpc.request",
            trace.id = trace_id,
            trace.producer_span_id = producer_span_id,
            http.request.method = %method,
            http.route = metric_route,
            "received AgentVault gRPC payment request"
        );
        if wire.headers.len() > MAX_HEADERS {
            return Err(Status::invalid_argument("too many payment request headers"));
        }
        let body = URL_SAFE_NO_PAD
            .decode(&wire.body)
            .map_err(|_| Status::invalid_argument("invalid payment request body encoding"))?;
        if body.len() > self.max_message_bytes {
            return Err(Status::resource_exhausted("payment request body is too large"));
        }
        let routed_uri = uri
            .path_and_query()
            .and_then(|value| value.as_str().strip_prefix(API_PREFIX))
            .ok_or_else(|| Status::invalid_argument("payment path must use the canonical API prefix"))?;
        let mut builder = Request::builder().method(method.clone()).uri(routed_uri);
        for (name, value) in wire.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| Status::invalid_argument("invalid payment request header"))?;
            if !GrpcEnvelopeService::is_allowed_request_header(&name) {
                return Err(Status::invalid_argument("unsupported payment request header"));
            }
            let value = HeaderValue::from_str(&value)
                .map_err(|_| Status::invalid_argument("invalid payment request header"))?;
            builder = builder.header(name, value);
        }
        let mut request = builder
            .body(Body::from(body))
            .map_err(|_| Status::invalid_argument("invalid payment request"))?;
        request.headers_mut().extend(trace_headers);
        request.extensions_mut().insert(principal);
        let routed = tokio::time::timeout(self.timeout, self.router.clone().oneshot(request))
            .await
            .map_err(|_| Status::deadline_exceeded("payment request timed out"))?;
        let response = match routed {
            Ok(response) => response,
            Err(never) => match never {},
        };
        let status = response.status().as_u16();
        self.metrics.record_http(metric_route, method.as_str(), status);
        let headers = response
            .headers()
            .iter()
            .filter_map(|(name, value)| value.to_str().ok().map(|value| (name.to_string(), value.to_owned())))
            .collect();
        let body = to_bytes(response.into_body(), self.max_message_bytes)
            .await
            .map_err(|_| Status::resource_exhausted("payment response body is too large"))?;
        let value = serde_json::to_vec(&WireResponse {
            status,
            headers,
            body: URL_SAFE_NO_PAD.encode(body),
        })
        .map_err(|_| Status::internal("encode payment response"))?;
        Ok(GrpcResponse::new(Envelope { value }))
    }

    async fn resolve_principal(&self, request: &GrpcRequest<Envelope>) -> Result<CallerPrincipal, Status> {
        let mut headers = HeaderMap::new();
        for name in ["x-authguard-context", "x-authguard-scope-token"] {
            if let Some(value) = request.metadata().get(name) {
                let value = value
                    .to_str()
                    .map_err(|_| Status::unauthenticated("invalid AuthGuard context metadata"))?;
                headers.insert(
                    HeaderName::from_static(name),
                    HeaderValue::from_str(value)
                        .map_err(|_| Status::unauthenticated("invalid AuthGuard context metadata"))?,
                );
            }
        }
        self.principals
            .resolve_request(&headers)
            .await
            .map_err(|error| match error {
                PrincipalResolutionError::Invalid => Status::unauthenticated("invalid AuthGuard caller context"),
                PrincipalResolutionError::Unavailable => Status::unavailable("AuthGuard scope resolver is unavailable"),
            })?
            .ok_or_else(|| Status::unauthenticated("trusted caller context is required"))
    }
}

impl GrpcEnvelopeService {
    fn trace_headers(request: &GrpcRequest<Envelope>) -> Result<HeaderMap, Status> {
        let mut headers = HeaderMap::new();
        for name in ["traceparent", "tracestate"] {
            if let Some(value) = request.metadata().get(name) {
                let value = value
                    .to_str()
                    .map_err(|_| Status::invalid_argument("invalid W3C trace context metadata"))?;
                headers.insert(
                    HeaderName::from_static(name),
                    HeaderValue::from_str(value)
                        .map_err(|_| Status::invalid_argument("invalid W3C trace context metadata"))?,
                );
            }
        }
        if GrpcEnvelopeService::trace_identity(&headers).is_none() {
            return Err(Status::invalid_argument("valid W3C traceparent metadata is required"));
        }
        Ok(headers)
    }

    fn trace_identity(headers: &HeaderMap) -> Option<(&str, &str)> {
        let value = headers.get("traceparent")?.to_str().ok()?;
        let mut fields = value.split('-');
        let version = fields.next()?;
        let trace_id = fields.next()?;
        let span_id = fields.next()?;
        let flags = fields.next()?;
        if fields.next().is_some()
            || version != "00"
            || !GrpcEnvelopeService::valid_lower_hex(trace_id, 32)
            || trace_id.bytes().all(|byte| byte == b'0')
            || !GrpcEnvelopeService::valid_lower_hex(span_id, 16)
            || span_id.bytes().all(|byte| byte == b'0')
            || !GrpcEnvelopeService::valid_lower_hex(flags, 2)
        {
            return None;
        }
        Some((trace_id, span_id))
    }

    fn valid_lower_hex(value: &str, length: usize) -> bool {
        value.len() == length
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
    }

    pub(crate) fn is_allowed_request_header(name: &HeaderName) -> bool {
        ALLOWED_REQUEST_HEADERS.contains(&name.as_str())
    }

    #[cfg(test)]
    fn is_allowed_payment_route(method: &Method, path: &str) -> bool {
        GrpcEnvelopeService::canonical_payment_route(method, path).is_some()
    }

    fn canonical_payment_route(method: &Method, path: &str) -> Option<&'static str> {
        if path.contains('\\') || path.contains("//") {
            return None;
        }
        match (method, path) {
            (&Method::POST, "/payments/authorize") => Some("/api/v1/payments/authorize"),
            (&Method::POST, "/prices/resolve") => Some("/api/v1/prices/resolve"),
            (&Method::POST, "/offers/resolve") => Some("/api/v1/offers/resolve"),
            (&Method::POST, "/payee/challenges") => Some("/api/v1/payee/challenges"),
            (&Method::POST, "/payee/verify") => Some("/api/v1/payee/verify"),
            (&Method::POST, "/payee/settle") => Some("/api/v1/payee/settle"),
            (&Method::GET, "/payments/balance") => Some("/api/v1/payments/balance"),
            (&Method::GET, "/approvals") => Some("/api/v1/approvals"),
            _ => {
                let segments = path.trim_start_matches('/').split('/').collect::<Vec<_>>();
                match (method, segments.as_slice()) {
                    (&Method::GET, ["payments", intent_id]) if !intent_id.is_empty() => {
                        Some("/api/v1/payments/{intent_id}")
                    }
                    (&Method::GET, ["approvals", approval_id]) if !approval_id.is_empty() => {
                        Some("/api/v1/approvals/{approval_id}")
                    }
                    (&Method::GET, ["payments", intent_id, "receipt"]) if !intent_id.is_empty() => {
                        Some("/api/v1/payments/{intent_id}/receipt")
                    }
                    (&Method::POST, ["payments", intent_id, "outbound-response"]) if !intent_id.is_empty() => {
                        Some("/api/v1/payments/{intent_id}/outbound-response")
                    }
                    (&Method::POST, ["approvals", approval_id, "decision"]) if !approval_id.is_empty() => {
                        Some("/api/v1/approvals/{approval_id}/decision")
                    }
                    _ => None,
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../../tests/transport/grpc_test.rs"]
mod tests;
