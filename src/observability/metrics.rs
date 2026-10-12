use std::sync::{Arc, Mutex};

use prometheus_client::{
    encoding::{text::encode, EncodeLabelSet},
    metrics::{
        counter::Counter,
        family::Family,
        gauge::Gauge,
        histogram::{exponential_buckets, Histogram},
    },
    registry::Registry,
};

use crate::model::sign::{SignOutcome, SignTransport};

pub trait MetricsRenderer: Send + Sync {
    /// Encodes the registered metrics in `OpenMetrics` text format.
    ///
    /// # Errors
    ///
    /// Returns a formatting error if a metric cannot be encoded.
    fn render_metrics(&self) -> Result<String, std::fmt::Error>;
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct SignLabels {
    transport: String,
    outcome: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct TransportLabels {
    transport: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct WalletLabels {
    operation: String,
    outcome: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct HttpLabels {
    route: String,
    method: String,
    status: String,
}

#[derive(Clone, Debug)]
pub struct AgentVaultMetrics {
    registry: Arc<Mutex<Registry>>,
    sign_requests: Family<SignLabels, Counter>,
    sign_duration: Family<TransportLabels, Histogram>,
    wallet_operations: Family<WalletLabels, Counter>,
    http_requests: Family<HttpLabels, Counter>,
    history_persistence_failures: Counter,
    history_records: Gauge,
    signs_in_flight: Gauge,
    mqtt_reconnects: Counter,
}

impl Default for AgentVaultMetrics {
    fn default() -> Self {
        let sign_requests = Family::default();
        let sign_duration = Family::<TransportLabels, Histogram>::new_with_constructor(sign_duration_histogram);
        let wallet_operations = Family::default();
        let http_requests = Family::default();
        let history_persistence_failures = Counter::default();
        let history_records = Gauge::default();
        let signs_in_flight = Gauge::default();
        let mqtt_reconnects = Counter::default();

        let mut registry = Registry::default();
        registry.register(
            "agentvault_sign_requests",
            "Payment signing requests by bounded transport and outcome",
            sign_requests.clone(),
        );
        registry.register(
            "agentvault_sign_duration_seconds",
            "Payment signing latency by transport",
            sign_duration.clone(),
        );
        registry.register(
            "agentvault_wallet_operations",
            "Wallet CRUD operations by bounded operation and outcome",
            wallet_operations.clone(),
        );
        registry.register(
            "agentvault_http_requests",
            "HTTP requests by stable route, method, and status",
            http_requests.clone(),
        );
        registry.register(
            "agentvault_history_persistence_failures",
            "Signing audit records that could not be persisted",
            history_persistence_failures.clone(),
        );
        registry.register(
            "agentvault_history_records",
            "Signing audit records retained by this process",
            history_records.clone(),
        );
        registry.register(
            "agentvault_signs_in_flight",
            "Signing operations currently executing",
            signs_in_flight.clone(),
        );
        registry.register(
            "agentvault_mqtt_reconnects",
            "MQTT event-loop interruptions that triggered reconnect handling",
            mqtt_reconnects.clone(),
        );
        Self {
            registry: Arc::new(Mutex::new(registry)),
            sign_requests,
            sign_duration,
            wallet_operations,
            http_requests,
            history_persistence_failures,
            history_records,
            signs_in_flight,
            mqtt_reconnects,
        }
    }
}

impl AgentVaultMetrics {
    pub fn sign_started(&self) {
        self.signs_in_flight.inc();
    }

    pub fn sign_finished(&self, transport: SignTransport, outcome: SignOutcome, seconds: f64) {
        self.signs_in_flight.dec();
        self.sign_requests
            .get_or_create(&SignLabels {
                transport: transport.as_str().to_owned(),
                outcome: outcome.as_str().to_owned(),
            })
            .inc();
        self.sign_duration
            .get_or_create(&TransportLabels {
                transport: transport.as_str().to_owned(),
            })
            .observe(seconds);
    }

    pub fn record_wallet(&self, operation: &str, succeeded: bool) {
        self.wallet_operations
            .get_or_create(&WalletLabels {
                operation: bounded_wallet_operation(operation).to_owned(),
                outcome: if succeeded { "success" } else { "failure" }.to_owned(),
            })
            .inc();
    }

    pub fn record_http(&self, route: &str, method: &str, status: u16) {
        self.http_requests
            .get_or_create(&HttpLabels {
                route: bounded_route(route).to_owned(),
                method: bounded_method(method).to_owned(),
                status: status.to_string(),
            })
            .inc();
    }

    pub fn history_persistence_failed(&self) {
        self.history_persistence_failures.inc();
    }

    pub fn set_history_records(&self, count: usize) {
        self.history_records.set(i64::try_from(count).unwrap_or(i64::MAX));
    }

    pub fn mqtt_reconnected(&self) {
        self.mqtt_reconnects.inc();
    }

    /// Encodes all metrics in `OpenMetrics` text format.
    ///
    /// # Errors
    ///
    /// Returns a formatting error if encoding fails.
    pub fn render(&self) -> Result<String, std::fmt::Error> {
        let registry = self.registry.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut body = String::new();
        encode(&mut body, &registry)?;
        Ok(body)
    }
}

impl MetricsRenderer for AgentVaultMetrics {
    fn render_metrics(&self) -> Result<String, std::fmt::Error> {
        self.render()
    }
}

fn bounded_wallet_operation(value: &str) -> &'static str {
    match value {
        "create" => "create",
        "get" => "get",
        "list" => "list",
        "rotate" => "rotate",
        "delete" => "delete",
        _ => "other",
    }
}

fn sign_duration_histogram() -> Histogram {
    Histogram::new(exponential_buckets(0.000_25, 2.0, 16))
}

fn bounded_method(value: &str) -> &'static str {
    match value {
        "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "DELETE" => "DELETE",
        _ => "OTHER",
    }
}

fn bounded_route(value: &str) -> &'static str {
    if value.ends_with("/wallet") {
        "/api/v1/wallet"
    } else if value.contains("/wallet/") {
        "/api/v1/wallet/{id}"
    } else if value.ends_with("/sign") {
        "/api/v1/sign"
    } else if value.contains("/sign/") {
        "/api/v1/sign/{request_id}"
    } else if value.ends_with("/healthz") {
        "/healthz"
    } else if value.ends_with("/readyz") {
        "/readyz"
    } else if value.ends_with("/metrics") {
        "/metrics"
    } else {
        "other"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_bounded_sign_metrics() {
        let metrics = AgentVaultMetrics::default();
        metrics.sign_started();
        metrics.sign_finished(SignTransport::Local, SignOutcome::Succeeded, 0.01);
        let rendered = metrics.render().unwrap();
        assert!(rendered.contains("agentvault_sign_requests_total{transport=\"local\",outcome=\"succeeded\"} 1"));
    }
}
