# AgentVault documentation

- [Architecture](architecture/overview.md): module boundaries, dependency
  direction, component lifecycle, persistence, and concurrency.
- [HTTP and signer API](api.md): wallet CRUD, payment-sign contract, history
  filters, and the Rust `IPaymentSigner` port.
- [Observability](observability.md): management endpoints, Prometheus metrics,
  OTLP traces, and label-safety rules.
- [Real-process use case](../use-cases/payment-signing/README.md): E2E scope and
  execution.

For setup and the security boundary, start with the repository
[README](../README.md).
