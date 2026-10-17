pub mod metrics;
pub mod pprof;
pub mod telemetry;

pub use metrics::{AgentVaultMetrics, MetricsRenderer};
pub use pprof::{runtime_snapshot, RuntimeProfile};
pub use telemetry::{init_telemetry, trace_http_request, TelemetryConfig, TelemetryGuard};
pub(crate) use telemetry::{inject_current_trace_context, set_span_parent_from_headers};

#[cfg(test)]
#[path = "../../tests/apm/mod_test.rs"]
mod tests;
