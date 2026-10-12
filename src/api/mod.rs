mod error;
mod sign;
mod wallet;

use std::{sync::Arc, time::Duration};

use anyhow::Context as _;
use async_trait::async_trait;
use axum::{
    body::Body,
    extract::{DefaultBodyLimit, MatchedPath, Request, State},
    http::{header, StatusCode},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::{
    config::ServerConfig,
    observability::{trace_http_request, AgentVaultMetrics},
    server::{RuntimeComponent, Shutdown},
    service::{PaymentSignService, WalletService},
};

pub use error::ApiError;

#[derive(Clone)]
pub struct ApiState {
    pub wallet: Arc<WalletService>,
    pub sign: Arc<PaymentSignService>,
}

#[derive(Clone)]
struct ApiMiddlewareState {
    token: Option<Arc<str>>,
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
        token: Option<String>,
        wallet: Arc<WalletService>,
        sign: Arc<PaymentSignService>,
        metrics: AgentVaultMetrics,
    ) -> Self {
        let state = ApiState { wallet, sign };
        let middleware_state = ApiMiddlewareState {
            token: token.map(Arc::from),
            timeout: config.request.timeout,
            metrics,
        };
        let endpoints = Router::new()
            .route("/wallet", get(wallet::list).post(wallet::create))
            .route(
                "/wallet/{id}",
                get(wallet::get).put(wallet::rotate).delete(wallet::delete),
            )
            .route("/sign", get(sign::search).post(sign::execute))
            .route("/sign/{request_id}", get(sign::get))
            .with_state(state);
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

async fn enforce_api_policy(State(state): State<ApiMiddlewareState>, request: Request<Body>, next: Next) -> Response {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| request.uri().path().to_owned(), |path| path.as_str().to_owned());
    let method = request.method().as_str().to_owned();
    if let Some(expected) = state.token.as_deref() {
        let supplied = request
            .headers()
            .get(header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "));
        if !supplied.is_some_and(|value| constant_time_eq(value.as_bytes(), expected.as_bytes())) {
            let response = ApiError::unauthorized().into_response();
            state.metrics.record_http(&route, &method, response.status().as_u16());
            return response;
        }
    }
    let response = match tokio::time::timeout(state.timeout, next.run(request)).await {
        Ok(response) => response,
        Err(_) => (StatusCode::REQUEST_TIMEOUT, "request timed out").into_response(),
    };
    state.metrics.record_http(&route, &method, response.status().as_u16());
    response
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    let mut difference = left.len() ^ right.len();
    let length = left.len().max(right.len());
    for index in 0..length {
        difference |= usize::from(left.get(index).copied().unwrap_or(0) ^ right.get(index).copied().unwrap_or(0));
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn compares_tokens_without_early_exit() {
        assert!(constant_time_eq(b"secret", b"secret"));
        assert!(!constant_time_eq(b"secret", b"secrex"));
        assert!(!constant_time_eq(b"secret", b"short"));
    }
}
