# Observability

The management plane follows the shared `mgmt:` shape used by AuthGuard:
separate listener, context path, health paths, metrics path, and OTLP settings.

Prometheus metrics use bounded labels only:

- `agentvault_sign_requests_total{transport,outcome}`
- `agentvault_sign_duration_seconds{transport}`
- `agentvault_signs_in_flight`
- `agentvault_wallet_operations_total{operation,outcome}`
- `agentvault_http_requests_total{route,method,status}`
- `agentvault_history_records`
- `agentvault_history_persistence_failures_total`
- `agentvault_mqtt_reconnects_total`

Wallet IDs, client IDs, request IDs, digests, and signatures are never metric
labels. The readiness endpoint validates both encrypted wallet metadata and the
history file; liveness only confirms that the process event loop responds.

Set `mgmt.otel.enabled: true` to export traces over OTLP/gRPC. The service emits
W3C-aware HTTP server spans and structured events. `OTEL_RESOURCE_ATTRIBUTES`
may provide `deployment.environment.name`. Logs support `HUMAN` and `JSON`
modes through `logging.mode`.
