# Architecture

AgentVault uses ports-and-adapters boundaries while remaining one deployable
binary. Domain contracts and services do not depend on Axum, Unix sockets, or
MQTT.

```text
src/
├── model/           stable wallet and payment-sign contracts
├── service/         wallet CRUD and signing use cases
├── signer/          external-client IPaymentSigner adapters
├── api/             HTTP product API adapter
├── transport/       inbound Unix-socket and MQTT adapters
├── management.rs    health and Prometheus management plane
├── observability/   bounded metrics and OTLP tracing
├── server/          component lifecycle and graceful shutdown
├── config/config.rs one typed configuration hierarchy
├── history.rs       durable, bounded audit metadata
└── store.rs         encrypted private-key persistence
```

## Dependency direction

`PaymentSignRequest` in `model/sign.rs` is the public contract. Inbound adapters
decode it and invoke `PaymentSignService`; the service validates protocol,
authorization policy, TTL, replay identity, and digest size before calling the
encrypted store. The service then records a redacted audit event and bounded
metrics. Transport-specific code never handles plaintext private keys.

The `IPaymentSigner` trait points in the other direction: it is the client-side
port consumed by Flowgent/Sigbot. `LocalPaymentSigner` and `MqttPaymentSigner`
implement the same `execute(request)` behavior, allowing a caller to select
deployment topology without changing payment-flow code.

## Runtime lifecycle

`AgentVaultServer` creates the store, history, services, and observability once.
It registers enabled HTTP API, management, local, and MQTT components with
`ServerSupervisor`. The supervisor owns process signals, broadcasts one
shutdown notification, waits up to `server.shutdown_timeout`, and reports the
first component failure. Components do not install competing signal handlers.

The product API and management API bind separate listeners. Management routes
are never merged into the product router.

## Persistence and concurrency

- The store holds one process-exclusive lock and serializes key mutations.
- Every private key has an independent authenticated ciphertext bound to its
  public metadata.
- Wallet rotation writes a complete replacement record atomically.
- Signing request IDs are cached for a bounded period. Identical redelivery
  returns the cached response; conflicting data under the same ID is rejected.
- Audit history is append-only JSON Lines during normal operation and compacts
  below its configured upper bound. It never contains digest or signature data.
