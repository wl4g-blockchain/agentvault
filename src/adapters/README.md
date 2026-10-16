# AgentVault application adapters

These five SDKs expose the same two integration points and keep payment state
machines out of business code:

| Language | Payer adapter | Payee adapter |
|---|---|---|
| Rust | `ReqwestPayerMiddleware` | `AxumPayeeMiddleware` |
| Go | `StandardPayerMiddleware` (`net/http`) | `GinPayeeMiddleware` / `StandardPayeeMiddleware` |
| Python | `HttpxPayerMiddleware` | `FastPayeeMiddleware` (ASGI/FastAPI) |
| TypeScript | `FetchPayerMiddleware` | `ExpressPayeeMiddleware` |
| Java | `StandardPayerMiddleware` (Spring client) | `SpringInterceptorPayeeMiddleware` |

`PayerMiddleware` owns 402 detection, stable operation identity, approval wait,
credential-origin binding, bounded retry, and settlement-response reporting.
`PayeeMiddleware` owns Offer-bound Challenge creation, Credential verification,
settlement, and paid-request context injection. Framework adapters only convert
native request and response types.

Each language exposes one `AgentVault` facade. Business code selects a
transport once, calls `vault.payer(config)` or `vault.payee(config)`, and then
attaches the ecosystem adapter with `standard()` or its framework-specific
method. There are no parallel `PaymentClient`, `PayeeClient`, or
`ApprovalClient` entrypoints; advanced operations remain methods on the role
middleware or the single facade.

Business applications choose one `VaultTransport` at startup:

- gRPC for synchronous service-to-service calls (TLS required off loopback);
- MQTT for durable client-scoped commands and approval events;
- HTTP when AgentVault is reached through the AuthGuard/Envoy edge.

With HTTP or gRPC, one transport instance can back both role middlewares and
the approval methods on `AgentVault`. MQTT exposes the payer/payee command flow
plus approval events, but not the administrative approval query/decision API.
No SDK accepts
a private key, arbitrary digest, `client_id`, or caller principal. Switching
transport does not change middleware configuration or application handlers.

The shared internal gRPC wire lives in `proto/transport.proto`. It deliberately
wraps the same bounded SDK DTO contract instead of publishing a second business
API surface.
