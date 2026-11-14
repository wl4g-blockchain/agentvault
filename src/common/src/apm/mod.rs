pub mod metrics;
mod pprof;
mod telemetry;

pub use metrics::AgentVaultMetrics;
pub use metrics::MetricsRenderer;
pub use pprof::runtime_snapshot;
pub use pprof::RuntimeProfile;
pub(crate) use telemetry::init_telemetry;
pub(crate) use telemetry::inject_current_trace_context;
pub(crate) use telemetry::trace_http_request;
pub(crate) use telemetry::TelemetryConfig;
pub use telemetry::TelemetryGuard;

#[cfg(test)]
#[path = "../../tests/apm/mod_test.rs"]
mod tests;

use axum::{extract::State, routing::get, Router};
use std::sync::Arc;
pub struct ApmComponent;
impl ApmComponent {
    pub fn management_router(
        config: &crate::config::ManagementConfig,
        metrics: crate::apm::AgentVaultMetrics,
        readiness_probes: Vec<Arc<dyn crate::apm::ReadinessProbe>>,
    ) -> Router {
        let state = ManagementState {
            metrics,
            readiness_probes,
            probe_timeout: config.health.probe_timeout,
        };
        let mut routes = Router::new()
            .route(
                &config.health.liveness_path,
                get(|| async { ManagementEndpoints::liveness() }),
            )
            .route(&config.health.readiness_path, get(ManagementEndpoints::readiness))
            .route(
                "/debug/runtime",
                get(|| async { ManagementEndpoints::runtime_profile() }),
            );
        if config.metrics.enabled {
            routes = routes.route(
                &config.metrics.path,
                get(|state: State<ManagementState>| async move { ManagementEndpoints::render_metrics(state) }),
            );
        }
        let routes = routes.with_state(state);
        if config.context_path == "/" {
            routes
        } else {
            Router::new().nest(&config.context_path, routes)
        }
    }
}

impl ApmComponent {
    ///
    /// # Errors
    /// Returns a telemetry configuration or exporter initialization failure.
    pub fn start(
        config: &crate::config::AppConfig,
        metrics: AgentVaultMetrics,
        probes: Vec<Arc<dyn ReadinessProbe>>,
        runtime: &mut crate::util::runtime::ServerSupervisor,
    ) -> anyhow::Result<TelemetryGuard> {
        let telemetry = init_telemetry(&TelemetryConfig::from_settings(
            &config.server.service_name,
            &config.logging,
            &config.mgmt.otel,
        ))?;
        if config.mgmt.enabled {
            runtime.add(ManagementServer::new(
                config.mgmt.clone(),
                Self::management_router(&config.mgmt, metrics, probes),
            ));
        }
        Ok(telemetry)
    }
}

use anyhow::Context as _;
use async_trait::async_trait;
use axum::{http::header, http::HeaderValue, http::StatusCode, response::IntoResponse, response::Response, Json};

use crate::{config::ManagementConfig, util::runtime::RuntimeComponent, util::runtime::Shutdown};

#[async_trait]
pub trait ReadinessProbe: Send + Sync {
    fn name(&self) -> &'static str;

    async fn check(&self) -> anyhow::Result<()>;
}

#[derive(Clone)]
pub(crate) struct ManagementState {
    pub(crate) metrics: AgentVaultMetrics,
    pub(crate) readiness_probes: Vec<std::sync::Arc<dyn ReadinessProbe>>,
    pub(crate) probe_timeout: std::time::Duration,
}

pub struct ManagementServer {
    config: ManagementConfig,
    router: Router,
}

impl ManagementServer {
    #[must_use]
    pub fn new(config: ManagementConfig, router: Router) -> Self {
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

#[cfg(test)]
#[path = "../../tests/apm/management_test.rs"]
mod management_tests;

pub(crate) struct ManagementEndpoints;
impl ManagementEndpoints {
    pub(crate) fn liveness() -> &'static str {
        "ok"
    }

    pub(crate) async fn readiness(State(state): State<ManagementState>) -> Response {
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

    pub(crate) fn render_metrics(State(state): State<ManagementState>) -> Response {
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

    pub(crate) fn runtime_profile() -> Json<crate::apm::RuntimeProfile> {
        Json(runtime_snapshot())
    }
}

impl ApmComponent {
    /// Propagates the current span to an outbound HTTP request.
    pub fn inject_current_trace_context(headers: &mut axum::http::HeaderMap) {
        telemetry::inject_current_trace_context(headers);
    }
}
