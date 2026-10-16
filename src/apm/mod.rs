pub mod metrics;
pub mod pprof;
pub mod telemetry;

pub use metrics::{AgentVaultMetrics, MetricsRenderer};
pub use pprof::{runtime_snapshot, RuntimeProfile};
pub use telemetry::{init_telemetry, trace_http_request, TelemetryConfig, TelemetryGuard};

#[cfg(test)]
#[path = "../../tests/apm/mod_test.rs"]
mod tests;
