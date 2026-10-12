use std::sync::Arc;

use anyhow::Context as _;
use async_trait::async_trait;
use axum::{
    extract::State,
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Router,
};

use crate::{
    config::ManagementConfig,
    observability::{AgentVaultMetrics, MetricsRenderer},
    server::{RuntimeComponent, Shutdown},
    service::PaymentSignService,
};

#[derive(Clone)]
struct ManagementState {
    metrics: AgentVaultMetrics,
    sign: Arc<PaymentSignService>,
}

pub struct ManagementServer {
    config: ManagementConfig,
    router: Router,
}

impl ManagementServer {
    #[must_use]
    pub fn new(config: ManagementConfig, metrics: AgentVaultMetrics, sign: Arc<PaymentSignService>) -> Self {
        let state = ManagementState { metrics, sign };
        let mut endpoints = Router::new()
            .route(&config.health.liveness_path, get(liveness))
            .route(&config.health.readiness_path, get(readiness));
        if config.metrics.enabled {
            endpoints = endpoints.route(&config.metrics.path, get(render_metrics));
        }
        let router = if config.context_path == "/" {
            endpoints.with_state(state)
        } else {
            Router::new().nest(&config.context_path, endpoints.with_state(state))
        };
        Self { config, router }
    }

    async fn serve(self, mut shutdown: Shutdown) -> anyhow::Result<()> {
        let address = std::net::SocketAddr::new(self.config.host, self.config.port);
        let listener = tokio::net::TcpListener::bind(address)
            .await
            .with_context(|| format!("bind management listener {address}"))?;
        tracing::info!(server.address = %address, "AgentVault management endpoints listening");
        axum::serve(listener, self.router)
            .with_graceful_shutdown(async move { shutdown.cancelled().await })
            .await
            .context("serve AgentVault management endpoints")
    }
}

#[async_trait]
impl RuntimeComponent for ManagementServer {
    fn name(&self) -> &'static str {
        "management HTTP server"
    }

    async fn run(self: Box<Self>, shutdown: Shutdown) -> anyhow::Result<()> {
        self.serve(shutdown).await
    }
}

async fn liveness() -> &'static str {
    "ok"
}

async fn readiness(State(state): State<ManagementState>) -> Response {
    match state.sign.readiness() {
        Ok(()) => (StatusCode::OK, "ready").into_response(),
        Err(error) => {
            tracing::warn!(%error, "service readiness check failed");
            (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response()
        }
    }
}

async fn render_metrics(State(state): State<ManagementState>) -> Response {
    match state.metrics.render_metrics() {
        Ok(body) => (
            [(
                header::CONTENT_TYPE,
                HeaderValue::from_static("application/openmetrics-text; version=1.0.0; charset=utf-8"),
            )],
            body,
        )
            .into_response(),
        Err(error) => {
            tracing::error!(%error, "failed to render Prometheus metrics");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}
