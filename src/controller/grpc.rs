use std::{collections::BTreeMap, net::SocketAddr, time::Duration};

use anyhow::Context as _;
use async_trait::async_trait;
use axum::{
    body::{to_bytes, Body},
    http::{HeaderMap, HeaderName, HeaderValue, Method, Request},
    Router,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use serde::{Deserialize, Serialize};
use tonic::{Request as GrpcRequest, Response as GrpcResponse, Status};
use tower::ServiceExt as _;

use crate::{
    cmd::{RuntimeComponent, Shutdown},
    config::GrpcTransportConfig,
    controller::http::PrincipalContext,
    grpc_wire::{
        payment_transport_server::{PaymentTransport, PaymentTransportServer},
        Envelope,
    },
};

const API_PREFIX: &str = "/api/v1";
const MAX_HEADERS: usize = 64;
const RESERVED_HEADERS: [&str; 3] = ["authorization", "host", "x-authguard-context"];

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
struct GrpcPaymentService {
    router: Router,
    principals: PrincipalContext,
    max_message_bytes: usize,
    timeout: Duration,
}

#[tonic::async_trait]
impl PaymentTransport for GrpcPaymentService {
    async fn execute(&self, request: GrpcRequest<Envelope>) -> Result<GrpcResponse<Envelope>, Status> {
        let principal = {
            let mut headers = HeaderMap::new();
            if let Some(value) = request.metadata().get("x-authguard-context") {
                let value = value
                    .to_str()
                    .map_err(|_| Status::unauthenticated("invalid AuthGuard context metadata"))?;
                headers.insert(
                    HeaderName::from_static("x-authguard-context"),
                    HeaderValue::from_str(value)
                        .map_err(|_| Status::unauthenticated("invalid AuthGuard context metadata"))?,
                );
            }
            self.principals
                .resolve(&headers)
                .ok_or_else(|| Status::unauthenticated("trusted caller context is required"))?
        };
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
        let path = wire
            .path
            .strip_prefix(API_PREFIX)
            .filter(|path| path.starts_with('/'))
            .ok_or_else(|| Status::invalid_argument("payment path must use the canonical API prefix"))?;
        if wire.headers.len() > MAX_HEADERS {
            return Err(Status::invalid_argument("too many payment request headers"));
        }
        let body = URL_SAFE_NO_PAD
            .decode(&wire.body)
            .map_err(|_| Status::invalid_argument("invalid payment request body encoding"))?;
        if body.len() > self.max_message_bytes {
            return Err(Status::resource_exhausted("payment request body is too large"));
        }
        let mut builder = Request::builder().method(method).uri(path);
        for (name, value) in wire.headers {
            let name = HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| Status::invalid_argument("invalid payment request header"))?;
            if is_reserved_request_header(&name) {
                return Err(Status::invalid_argument("reserved payment request header"));
            }
            let value = HeaderValue::from_str(&value)
                .map_err(|_| Status::invalid_argument("invalid payment request header"))?;
            builder = builder.header(name, value);
        }
        let mut request = builder
            .body(Body::from(body))
            .map_err(|_| Status::invalid_argument("invalid payment request"))?;
        request.extensions_mut().insert(principal);
        let routed = tokio::time::timeout(self.timeout, self.router.clone().oneshot(request))
            .await
            .map_err(|_| Status::deadline_exceeded("payment request timed out"))?;
        let response = match routed {
            Ok(response) => response,
            Err(never) => match never {},
        };
        let status = response.status().as_u16();
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
}

pub(super) fn is_reserved_request_header(name: &HeaderName) -> bool {
    RESERVED_HEADERS.contains(&name.as_str())
}

pub struct GrpcController {
    config: GrpcTransportConfig,
    service: GrpcPaymentService,
}

impl GrpcController {
    #[must_use]
    pub fn new(config: GrpcTransportConfig, principals: PrincipalContext, router: Router) -> Self {
        let service = GrpcPaymentService {
            router,
            principals,
            max_message_bytes: config.max_message_bytes,
            timeout: config.request_timeout,
        };
        Self { config, service }
    }
}

#[async_trait]
impl RuntimeComponent for GrpcController {
    fn name(&self) -> &'static str {
        "gRPC payment transport"
    }

    async fn run(self: Box<Self>, mut shutdown: Shutdown) -> anyhow::Result<()> {
        let address = SocketAddr::new(self.config.host, self.config.port);
        let service = PaymentTransportServer::new(self.service)
            .max_decoding_message_size(self.config.max_message_bytes)
            .max_encoding_message_size(self.config.max_message_bytes);
        tracing::info!(server.address = %address, "AgentVault gRPC transport listening");
        tonic::transport::Server::builder()
            .add_service(service)
            .serve_with_shutdown(address, async move { shutdown.cancelled().await })
            .await
            .with_context(|| format!("serve gRPC payment transport on {address}"))
    }
}

#[cfg(test)]
#[path = "../../tests/controller/grpc_test.rs"]
mod tests;
