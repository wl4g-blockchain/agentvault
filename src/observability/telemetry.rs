use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};
use opentelemetry::{global, propagation::Extractor, trace::TracerProvider as _, KeyValue};
use opentelemetry_otlp::WithExportConfig as _;
use opentelemetry_sdk::{
    propagation::TraceContextPropagator,
    trace::{Sampler, SdkTracerProvider},
    Resource,
};
use tracing::Instrument as _;
use tracing_opentelemetry::OpenTelemetrySpanExt as _;
use tracing_subscriber::{filter::EnvFilter, fmt, layer::SubscriberExt as _, util::SubscriberInitExt as _, Registry};

use crate::config::{LoggingConfig, OtelConfig};

#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    pub service_name: String,
    pub service_version: String,
    pub environment: String,
    pub env_filter: String,
    pub json: bool,
    pub otlp_endpoint: Option<String>,
    pub export_timeout: Duration,
    pub sample_rate: f64,
}

impl TelemetryConfig {
    #[must_use]
    pub fn from_settings(service_name: &str, logging: &LoggingConfig, otel: &OtelConfig) -> Self {
        Self {
            service_name: service_name.to_owned(),
            service_version: env!("CARGO_PKG_VERSION").to_owned(),
            environment: std::env::var("OTEL_RESOURCE_ATTRIBUTES")
                .ok()
                .and_then(|attributes| find_resource_attribute(&attributes, "deployment.environment.name"))
                .unwrap_or_else(|| "production".to_owned()),
            env_filter: logging.level.clone(),
            json: logging.mode.eq_ignore_ascii_case("JSON"),
            otlp_endpoint: otel.enabled.then(|| otel.endpoint.clone()),
            export_timeout: otel.timeout,
            sample_rate: otel.sample_rate,
        }
    }
}

#[derive(Debug, Default)]
pub struct TelemetryGuard {
    provider: Option<SdkTracerProvider>,
}

impl TelemetryGuard {
    pub fn shutdown(mut self) {
        if let Some(provider) = self.provider.take() {
            let _ = provider.shutdown();
        }
    }
}

impl Drop for TelemetryGuard {
    fn drop(&mut self) {
        if let Some(provider) = self.provider.take() {
            let _ = provider.shutdown();
        }
    }
}

/// Installs structured tracing and an optional OTLP/gRPC trace exporter.
///
/// # Errors
///
/// Returns an error if exporter construction or subscriber installation fails.
pub fn init_telemetry(config: &TelemetryConfig) -> anyhow::Result<TelemetryGuard> {
    global::set_text_map_propagator(TraceContextPropagator::new());
    let filter = EnvFilter::try_new(&config.env_filter).unwrap_or_else(|_| EnvFilter::new("info"));
    let provider = if let Some(endpoint) = config.otlp_endpoint.as_deref() {
        let exporter = opentelemetry_otlp::SpanExporter::builder()
            .with_tonic()
            .with_endpoint(endpoint)
            .with_timeout(config.export_timeout)
            .build()?;
        let resource = Resource::builder()
            .with_service_name(config.service_name.clone())
            .with_attributes([
                KeyValue::new("service.version", config.service_version.clone()),
                KeyValue::new("deployment.environment.name", config.environment.clone()),
            ])
            .build();
        Some(
            SdkTracerProvider::builder()
                .with_resource(resource)
                .with_sampler(Sampler::TraceIdRatioBased(config.sample_rate))
                .with_batch_exporter(exporter)
                .build(),
        )
    } else {
        None
    };

    match (provider.as_ref(), config.json) {
        (Some(provider), true) => Registry::default()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("agentvault")))
            .with(filter)
            .with(fmt::layer().json())
            .try_init()?,
        (Some(provider), false) => Registry::default()
            .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("agentvault")))
            .with(filter)
            .with(fmt::layer())
            .try_init()?,
        (None, true) => Registry::default().with(filter).with(fmt::layer().json()).try_init()?,
        (None, false) => Registry::default().with(filter).with(fmt::layer()).try_init()?,
    }
    tracing::info!(
        service.name = %config.service_name,
        service.version = %config.service_version,
        deployment.environment.name = %config.environment,
        otel.enabled = provider.is_some(),
        "telemetry initialized"
    );
    Ok(TelemetryGuard { provider })
}

struct HeaderExtractor<'a>(&'a axum::http::HeaderMap);

impl Extractor for HeaderExtractor<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        self.0.get(key)?.to_str().ok()
    }

    fn keys(&self) -> Vec<&str> {
        self.0.keys().map(axum::http::HeaderName::as_str).collect()
    }
}

/// Extracts W3C Trace Context and creates one server span around an HTTP request.
pub async fn trace_http_request(request: Request<Body>, next: Next) -> Response {
    const MAX_REQUEST_ID_BYTES: usize = 128;

    let started = Instant::now();
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| request.uri().path().to_owned(), |path| path.as_str().to_owned());
    let request_id = request
        .headers()
        .get("x-request-id")
        .and_then(|value| value.to_str().ok())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= MAX_REQUEST_ID_BYTES
                && value.bytes().all(|byte| byte.is_ascii_graphic())
        })
        .unwrap_or("")
        .to_owned();
    let parent = global::get_text_map_propagator(|propagator| propagator.extract(&HeaderExtractor(request.headers())));
    let span = tracing::info_span!(
        "http.server.request",
        otel.kind = "server",
        http.request.method = %method,
        http.request.id = %request_id,
        http.route = %route,
        http.response.status_code = tracing::field::Empty,
    );
    let _ = span.set_parent(parent);
    let response = async move {
        let response = next.run(request).await;
        tracing::debug!(
            http.response.status_code = response.status().as_u16(),
            duration_seconds = started.elapsed().as_secs_f64(),
            "HTTP request completed"
        );
        response
    }
    .instrument(span.clone())
    .await;
    span.record("http.response.status_code", response.status().as_u16());
    response
}

fn find_resource_attribute(attributes: &str, name: &str) -> Option<String> {
    attributes.split(',').find_map(|entry| {
        let (key, value) = entry.split_once('=')?;
        (key.trim() == name).then(|| value.trim().to_owned())
    })
}
