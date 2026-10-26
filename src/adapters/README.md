# AgentVault application adapters

These five SDKs expose the same two integration points and keep payment state
machines out of business code:

| Language | Payer adapter | Payee adapter |
|---|---|---|
| Rust | `ReqwestPayerMiddleware` | `AxumPayeeMiddleware` |
| Go | `StandardPayerMiddleware` (`net/http`) | `GinPayeeMiddleware` / `StdHttpMiddleware` |
| Python | `HttpxPayerMiddleware` | `FastPayeeMiddleware` (ASGI/FastAPI) |
| TypeScript | `FetchPayerMiddleware` | `ExpressPayeeMiddleware` |
| Java | `JdkHttpPayerMiddleware` / `SpringRestPayerMiddleware` (MVC/WebFlux) | `SpringInterceptorPayeeMiddleware` |

`PayerMiddleware` owns 402 detection, stable operation identity, approval wait,
credential-origin binding, bounded retry, and settlement-response reporting.
`PayeeMiddleware` owns Price-bound Challenge creation, Credential verification,
settlement, and paid-request context injection. Framework adapters only convert
native request and response types.

Each language exposes one `AgentVault` facade. Business code selects a
transport once, calls `vault.payer(config)` or `vault.payee(config)`, and then
attaches the ecosystem adapter with `standard()` or its explicitly named framework-specific
method. There are no parallel `PaymentClient`, `PayeeClient`, or
`ApprovalClient` entrypoints; advanced operations remain methods on the role
middleware or the single facade.

Business applications choose one `VaultTransport` at startup:

- gRPC for synchronous service-to-service calls (TLS required off loopback);
- MQTT for asynchronous client-scoped commands and correlated results;

With gRPC, one transport instance can back both role middlewares and
the approval methods on `AgentVault`. MQTT exposes only the payer/payee command
flow and correlated results to workload credentials; approval events and the
administrative query/decision API require a separate Admin identity.
No SDK accepts
a private key, arbitrary digest, `client_id`, or caller principal. Switching
transport does not change middleware configuration or application handlers.

MQTT keeps one connection and callback/event loop per transport instance. Go
exposes `DoAsync`, Python `request_async`, Java `getAsync`/`postAsync`, and
Rust/TypeScript return Futures/Promises. Request/response entrypoints wait
on that same asynchronous implementation. Both commands and result subscriptions
require QoS 1; a rejected or downgraded subscription fails closed.

After reconnect, the transport restores the result subscription before replaying
pending requests with identical envelope bytes and `requestSpanId`. The Vault
receiver coalesces identical deliveries, rejects conflicting reuse, and preserves
database idempotency through `run_id`. Deadlines include connection setup; pending
requests are bounded, cancellation removes local waiters, and close is idempotent.
Cancellation or timeout cannot undo a command already delivered to Vault: payment
recovery must retain the original `run_id`. Pending SDK queues are in memory;
financial recovery after process restart remains Vault's responsibility.

The shared internal gRPC wire lives in `proto/transport.proto`. It deliberately
wraps the same bounded SDK DTO contract instead of publishing a second business
API surface.


## A2A automatic payments

All five SDKs intercept A2A **0.3 JSON-RPC `message/send`** in their existing
HTTP adapters. Explicit payer operation context enables payment; ordinary calls
remain unchanged. The same payer orchestration performs authorization, approval,
signing, bounded delivery and receipt reporting for HTTP, MCP and A2A.

- x402 uses v2 upstream models and the [A2A x402 v0.2 extension](https://github.com/google-agentic-commerce/a2a-x402/blob/main/spec/v0.2/spec.md):
  `input-required` Task, `x402.payment.required`, message `x402.payment.payload`,
  and status-message `x402.payment.receipts`. The client activates and server
  acknowledges `X-A2A-Extensions`; publish the extension on the AgentCard too.
- MPP uses the upstream JSON-RPC `-32042`/`-32043` challenge and root
  `_meta["org.paymentauth/credential"]` / `_meta["org.paymentauth/receipt"]`.
  It does not reuse the x402 extension or its payload shape. Vault supports the
  configured Tempo charge method; unsupported methods remain rejected.
- A plain HTTP 402 at an A2A endpoint still uses the HTTP provider path. Payment
  never changes protocol or carrier after signing. Bearer authentication remains
  intact; message credentials do not occupy the Authorization header.

The host supplies **its existing TaskStore** and authorizes access to tasks before
the payment middleware. Do not expose another tenant's tasks through that store.
The SDK preserves existing task/context/message IDs and application metadata,
and records the run/scenario/price binding in SDK-owned Task metadata. A task
created by the payment interceptor uses `run_id` as its stable Task ID; existing
Task IDs may differ. The same paid task cannot be rebound to another run or price.
Use a durable host TaskStore for restart recovery. Rust's upstream `a2a-types`
provides wire types only, so implement the SDK's small `A2aTaskStore` port over the
host store; the other languages directly use their upstream TaskStore contract.

The handler receives a credential-stripped request after Vault verify/settle,
and must return the bound native Task. Settlement receipt metadata is owned by
middleware: handler-supplied receipts and changed task/context bindings fail
closed. A JSON-RPC business error after settlement becomes a failed Task carrying
that error and the confirmed receipt. Payer reports that receipt to Vault as a
locator; Vault still requires independent settlement evidence before accounting.
A2A Task completion and payment settlement are separate outcomes.

Streaming (`message/stream`), the A2A 1.0 method names and nested AP2 payments are
explicitly unsupported for automatic payment. They are never replayed or silently
converted into `message/send`. This binding matches the A2A 0.3 integration used
by Flowgent; it does not claim support for every A2A version or transport binding.

### Attach to existing application adapters

Construct `vault` through the SDK's typed `AgentVault.from_config` /
`fromConfig` / `NewFromConfig` entrypoint. In these snippets `store` is the
application's TaskStore, `route` contains its existing scenario, price, resource
and origin, and `request` contains an upstream `MessageSendParams` JSON-RPC body.
Selecting MQTT versus gRPC in Vault configuration does not change this code.

| SDK | Payee attachment | Payer attachment |
|---|---|---|
| Go | Set `StandardPayeeRoute.A2ATaskStore` or `GinPayeeRoute.A2ATaskStore`; call `core.Standard(route).Handler(handler)` or `core.Gin(route).Handler()` | `core.Standard(nil).Client().Do(payer.WithPayment(request, operation))` |
| Rust | `core.axum(vec![route]).with_a2a_task_store("/a2a", store)`; leave `route.mcp_tool` unset | `core.standard().client()?.post(url).header("content-type", "application/json").body(body).with_payment(operation).send().await` |
| Python | `FastPayeeRoute(..., a2a_task_store=store)` with `core.fast(app, routes)` | `await core.standard().post(url, json=request, operation=operation)` |
| TypeScript | Set `a2aTaskStore: store` on `core.express([route])`; mount its `handlers()` before the application's JSON body parser | `await core.standard().request(url, {method:'POST', headers:{'content-type':'application/json'}, body:JSON.stringify(request)}, operation)` |
| Java | `SpringInterceptorPayeeMiddleware.Route.a2a(path, scenario, price, resource, origin, protocol, store)` through `core.spring(routes)` | `core.jdkHttp().send(request, body, operation)` or the existing Spring Rest/WebFlux adapters |

Use `PayeeMiddleware.A2AAgentCard` (Go), `a2a_agent_card` (Rust), or
`core.a2a().agent_card/agentCard` (Python/TypeScript/Java) to declare the enabled
extension on the application's upstream AgentCard. Authentication and task
ownership stay with the host application.

### Reproducible SDK acceptance

The shared [A2A fixture](tests/payment_protocol_a2a.json) defines native x402/MPP
messages and the complete MQTT/gRPC × protocol × role contract. Mirrored
`model/a2a`, `payer/a2a`, `payee/a2a` tests in each SDK execute both sides through
the actual HTTP adapters. They cover Task continuation, stable operation hashes,
Bearer preservation, extension negotiation, protocol conflicts, unsupported calls
and receipts after business failure. Java covers JDK, MVC and WebFlux; Go covers
net/http and Gin. These tests use a controlled Vault port and are **SDK boundary
acceptance**, not proof of chain settlement or new mainnet transactions.

Run them together through the existing E2E entrypoint after installing each SDK's
locked dependencies:

```sh
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s a2a --timeout 600
```

The separate `mpp-tempo`, `x402-solana` and full MarketPulse scenarios validate
production Vault custody, SQL transactions, local-chain transfers, reconciliation
and balance/gas conservation. Their results must not be replaced by the shared
wire fixture or the SDK boundary tests.
