use anyhow::Context as _;
use async_trait::async_trait;
use axum::{
    extract::State,
    http::{header, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};

use crate::{
    apm::{runtime_snapshot, AgentVaultMetrics, MetricsRenderer},
    cmd::{RuntimeComponent, Shutdown},
    config::ManagementConfig,
};

#[derive(Clone)]
struct ManagementState {
    metrics: AgentVaultMetrics,
}

pub struct ManagementServer {
    config: ManagementConfig,
    router: Router,
}

impl ManagementServer {
    #[must_use]
    pub fn new(config: ManagementConfig, metrics: AgentVaultMetrics) -> Self {
        let state = ManagementState { metrics };
        let mut endpoints = Router::new()
            .route(&config.health.liveness_path, get(liveness))
            .route(&config.health.readiness_path, get(readiness))
            .route("/debug/runtime", get(runtime_profile));
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

async fn readiness() -> Response {
    (StatusCode::OK, "ready").into_response()
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

async fn runtime_profile() -> Json<crate::apm::RuntimeProfile> {
    Json(runtime_snapshot())
}

#[cfg(test)]
#[path = "../../tests/controller/management_test.rs"]
mod tests;
