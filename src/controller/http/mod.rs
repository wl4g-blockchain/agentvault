mod error;
mod payment;
mod principal;

use std::time::Duration;

use anyhow::Context as _;
use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, MatchedPath, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    Router,
};

use crate::{
    apm::{trace_http_request, AgentVaultMetrics},
    cmd::{RuntimeComponent, Shutdown},
    config::ServerConfig,
};

pub use error::ApiError;
pub use payment::{payment_router, PaymentApi, PaymentApiDependencies};
pub(crate) use payment::{
    AuthorizePaymentRequest, AuthorizePaymentResponse, CreateChallengeRequest, ReportOutboundResponseRequest,
    ReportOutboundResponseResponse, ResolveOfferRequest, SettlePaymentRequest, SubmitApprovalDecisionRequest,
    VerifyPaymentRequest,
};
pub(crate) use principal::PrincipalResolutionError;
pub use principal::{CallerPrincipal, PrincipalContext};

#[derive(Clone)]
struct ApiMiddlewareState {
    principal_context: PrincipalContext,
    timeout: Duration,
    metrics: AgentVaultMetrics,
}

pub struct HttpApiServer {
    config: ServerConfig,
    router: Router,
}

impl HttpApiServer {
    #[must_use]
    pub fn new(
        config: ServerConfig,
        principal_context: PrincipalContext,
        endpoints: Router,
        metrics: AgentVaultMetrics,
    ) -> Self {
        let middleware_state = ApiMiddlewareState {
            principal_context,
            timeout: config.request.timeout,
            metrics,
        };
        let routes = if config.context_path == "/" {
            endpoints
        } else {
            Router::new().nest(&config.context_path, endpoints)
        }
        .layer(DefaultBodyLimit::max(config.request.max_body_bytes))
        .layer(middleware::from_fn(trace_http_request))
        .layer(middleware::from_fn_with_state(middleware_state, enforce_api_policy));
        Self { config, router: routes }
    }

    async fn serve(self, mut shutdown: Shutdown) -> anyhow::Result<()> {
        let address = std::net::SocketAddr::new(self.config.host, self.config.port);
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .with_context(|| format!("bind API listener {address}"))?;
        tracing::info!(server.address = %address, "AgentVault API listening");
        axum::serve(listener, self.router)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await
            .context("serve AgentVault API")
    }
}

#[async_trait]
impl RuntimeComponent for HttpApiServer {
    fn name(&self) -> &'static str {
        "HTTP API"
    }

    async fn run(self: Box<Self>, shutdown: Shutdown) -> anyhow::Result<()> {
        self.serve(shutdown).await
    }
}

#[cfg(test)]
#[path = "../../../tests/controller/http/mod_test.rs"]
mod tests;

async fn enforce_api_policy(State(state): State<ApiMiddlewareState>, request: Request<Body>, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| request.uri().path().to_owned(), |path| path.as_str().to_owned());
    let method = request.method().as_str().to_owned();
    let principal = match state.principal_context.resolve_request(request.headers()).await {
        Ok(Some(principal)) => principal,
        Ok(None) | Err(PrincipalResolutionError::Invalid) => {
            let response = ApiError::unauthorized().into_response();
            state.metrics.record_http(&route, &method, response.status().as_u16());
            return response;
        }
        Err(PrincipalResolutionError::Unavailable) => {
            let response = ApiError::dependency_unavailable().into_response();
            state.metrics.record_http(&route, &method, response.status().as_u16());
            return response;
        }
    };
    let mut request = request;
    request.extensions_mut().insert(principal);
    let response = match tokio::time::timeout(state.timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => (StatusCode::REQUEST_TIMEOUT, "request timed out").into_response(),
    };
    state.metrics.record_http(&route, &method, response.status().as_u16());
    response
}
