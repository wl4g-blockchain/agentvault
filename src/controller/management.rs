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

#[async_trait]
pub(crate) trait ReadinessProbe: Send + Sync {
    fn name(&self) -> &'static str;

    async fn check(&self) -> anyhow::Result<()>;
}

#[derive(Clone)]
struct ManagementState {
    metrics: AgentVaultMetrics,
    readiness_probes: Vec<std::sync::Arc<dyn ReadinessProbe>>,
    probe_timeout: std::time::Duration,
}

pub struct ManagementServer {
    config: ManagementConfig,
    router: Router,
}

impl ManagementServer {
    #[must_use]
    pub fn new(config: ManagementConfig, metrics: AgentVaultMetrics) -> Self {
        Self::with_readiness_probes(config, metrics, Vec::new())
    }

    #[must_use]
    pub(crate) fn with_readiness_probes(
        config: ManagementConfig,
        metrics: AgentVaultMetrics,
        readiness_probes: Vec<std::sync::Arc<dyn ReadinessProbe>>,
    ) -> Self {
        let state = ManagementState {
            metrics,
            readiness_probes,
            probe_timeout: config.health.probe_timeout,
        };
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

async fn readiness(State(state): State<ManagementState>) -> Response {
    for probe in &state.readiness_probes {
        match tokio::time::timeout(state.probe_timeout, probe.check()).await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                tracing::warn!(dependency = probe.name(), %error, "readiness dependency is unavailable");
                return (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response();
            }
            Err(_) => {
                tracing::warn!(
                    dependency = probe.name(),
                    timeout_ms = u64::try_from(state.probe_timeout.as_millis()).unwrap_or(u64::MAX),
                    "readiness dependency timed out"
                );
                return (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response();
            }
        }
    }
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
