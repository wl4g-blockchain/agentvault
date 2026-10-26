use std::{
    sync::{Arc, Mutex},
    time::Instant,
};

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

pub trait MetricsRenderer: Send + Sync {
    /// Encodes the registered metrics in `OpenMetrics` text format.
    ///
    /// # Errors
    ///
    /// Returns a formatting error if a metric cannot be encoded.
    fn render_metrics(&self) -> Result<String, std::fmt::Error>;
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct PaymentLabels {
    operation: String,
    direction: String,
    outcome: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct PaymentDurationLabels {
    operation: String,
    direction: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct HttpLabels {
    route: String,
    method: String,
    status: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct WalletSelectionLabels {
    strategy: String,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq, EncodeLabelSet)]
struct BalanceSyncLabels {
    network: String,
    outcome: String,
}

#[derive(Clone, Debug)]
pub struct AgentVaultMetrics {
    registry: Arc<Mutex<Registry>>,
    payment_operations: Family<PaymentLabels, Counter>,
    payment_duration: Family<PaymentDurationLabels, Histogram>,
    http_requests: Family<HttpLabels, Counter>,
    payments_in_flight: Gauge,
    wallet_selections: Family<WalletSelectionLabels, Counter>,
    wallet_balance_sync: Family<BalanceSyncLabels, Counter>,
}

/// Fail-safe timer for one transport-neutral payment operation. Unless the
/// application service explicitly marks completion, dropping the guard records
/// a failure; early returns therefore cannot silently disappear from metrics.
#[must_use]
#[derive(Debug)]
pub struct PaymentOperationGuard {
    metrics: AgentVaultMetrics,
    operation: &'static str,
    direction: &'static str,
    started: Instant,
    succeeded: bool,
}

impl PaymentOperationGuard {
    /// Marks the guarded operation as successfully completed.
    pub fn succeed(&mut self) {
        self.succeeded = true;
    }
}

impl Drop for PaymentOperationGuard {
    fn drop(&mut self) {
        self.metrics.payment_finished(
            self.operation,
            self.direction,
            self.succeeded,
            self.started.elapsed().as_secs_f64(),
        );
    }
}

impl Default for AgentVaultMetrics {
    fn default() -> Self {
        let payment_operations = Family::default();
        let payment_duration =
            Family::<PaymentDurationLabels, Histogram>::new_with_constructor(payment_duration_histogram);
        let http_requests = Family::default();
        let payments_in_flight = Gauge::default();
        let wallet_selections = Family::default();
        let wallet_balance_sync = Family::default();

        let mut registry = Registry::default();
        registry.register(
            "agentvault_payment_operations",
            "Payment operations by bounded operation, direction, and outcome",
            payment_operations.clone(),
        );
        registry.register(
            "agentvault_payment_duration_seconds",
            "Payment operation latency by bounded operation and direction",
            payment_duration.clone(),
        );
        registry.register(
            "agentvault_http_requests",
            "HTTP requests by stable route, method, and status",
            http_requests.clone(),
        );
        registry.register(
            "agentvault_payments_in_flight",
            "Payment operations currently executing",
            payments_in_flight.clone(),
        );
        registry.register(
            "agentvault_wallet_selections",
            "Outbound wallet selections by configured strategy",
            wallet_selections.clone(),
        );
        registry.register(
            "agentvault_wallet_balance_sync",
            "Periodic managed-wallet balance observations by network and outcome",
            wallet_balance_sync.clone(),
        );
        Self {
            registry: Arc::new(Mutex::new(registry)),
            payment_operations,
            payment_duration,
            http_requests,
            payments_in_flight,
            wallet_selections,
            wallet_balance_sync,
        }
    }
}

impl AgentVaultMetrics {
    pub fn payment_operation(&self, operation: &'static str, direction: &'static str) -> PaymentOperationGuard {
        self.payment_started();
        PaymentOperationGuard {
            metrics: self.clone(),
            operation,
            direction,
            started: Instant::now(),
            succeeded: false,
        }
    }

    pub fn payment_started(&self) {
        self.payments_in_flight.inc();
    }

    pub fn payment_finished(&self, operation: &str, direction: &str, succeeded: bool, seconds: f64) {
        self.payments_in_flight.dec();
        let operation = bounded_payment_operation(operation);
        let direction = bounded_direction(direction);
        self.payment_operations
            .get_or_create(&PaymentLabels {
                operation: operation.to_owned(),
                direction: direction.to_owned(),
                outcome: if succeeded { "success" } else { "failure" }.to_owned(),
            })
            .inc();
        self.payment_duration
            .get_or_create(&PaymentDurationLabels {
                operation: operation.to_owned(),
                direction: direction.to_owned(),
            })
            .observe(seconds);
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

    pub fn record_wallet_selection(&self, strategy: &str) {
        let strategy = match strategy {
            "least_spent" => "least_spent",
            "stable_random" => "stable_random",
            _ => "other",
        };
        self.wallet_selections
            .get_or_create(&WalletSelectionLabels {
                strategy: strategy.to_owned(),
            })
            .inc();
    }

    pub fn record_wallet_balance_sync(&self, network: &str, succeeded: bool) {
        self.wallet_balance_sync
            .get_or_create(&BalanceSyncLabels {
                network: network.to_owned(),
                outcome: if succeeded { "success" } else { "failure" }.to_owned(),
            })
            .inc();
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

fn bounded_payment_operation(value: &str) -> &'static str {
    match value {
        "authorize" => "authorize",
        "approve" => "approve",
        "sign" => "sign",
        "verify" => "verify",
        "settle" => "settle",
        "reconcile" => "reconcile",
        _ => "other",
    }
}

fn bounded_direction(value: &str) -> &'static str {
    match value {
        "inbound" => "inbound",
        "outbound" => "outbound",
        _ => "other",
    }
}

fn payment_duration_histogram() -> Histogram {
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
    if value.ends_with("/payments/authorize") {
        "/api/v1/payments/authorize"
    } else if value.ends_with("/payments/balance") {
        "/api/v1/payments/balance"
    } else if value.ends_with("/payments/{intent_id}/receipt") {
        "/api/v1/payments/{intent_id}/receipt"
    } else if value.ends_with("/payments/{intent_id}") {
        "/api/v1/payments/{intent_id}"
    } else if value.ends_with("/payments/{intent_id}/outbound-response") {
        "/api/v1/payments/{intent_id}/outbound-response"
    } else if value.ends_with("/approvals/{approval_id}/decision") {
        "/api/v1/approvals/{approval_id}/decision"
    } else if value.ends_with("/approvals/{approval_id}") {
        "/api/v1/approvals/{approval_id}"
    } else if value.ends_with("/approvals") {
        "/api/v1/approvals"
    } else if value.ends_with("/prices/resolve") {
        "/api/v1/prices/resolve"
    } else if value.ends_with("/payee/challenges") {
        "/api/v1/payee/challenges"
    } else if value.ends_with("/payee/verify") {
        "/api/v1/payee/verify"
    } else if value.ends_with("/payee/settle") {
        "/api/v1/payee/settle"
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
#[path = "../../tests/apm/metrics_test.rs"]
mod tests;
