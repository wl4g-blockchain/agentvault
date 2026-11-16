# AgentVault architecture overview

[中文](overview_ZH.md) · [Documentation index](../index.md)

AgentVault is bidirectional payment infrastructure for AI agents. Business applications embed Payer or Payee SDKs and call AgentVault over standard HTTP. AgentVault owns payment authorization, wallet custody, approvals, settlement verification, immutable accounting, and crash recovery. Applications retain ownership of workflows, resources, tasks, and delivery.

This overview describes implemented service and deployment boundaries. Protocol-specific contracts belong to the [SDK documentation](../../src/sdk/README.md).

## 1. Logical architecture and control boundaries

![AgentVault logical architecture: UI, AuthGuard, business SDKs, payment services, external settlement, and infrastructure](assets/agentvault-logical-architecture.svg)

### Numbered invocation paths

Every connection has a sequence ID. The same prefix identifies a call flow; numbers order that flow rather than imposing a global clock on independent requests. Replies use the same connection. The physical view collapses in-process interactions.

| Flow | Call order |
|---|---|
| Access | **A1** client request → **A2** Gateway/AuthGuard access check → **A3** protected API routing, or **A4** static UI routing in the physical view. SDK entry P2/I1 reuses A2/A3. |
| Outbound payment | **P1** resource request/402 → **P2** Payer SDK calls Gateway → **P3** Payer route → **P4** domain authorization/reservation → **P5** bound protocol plan → **P6** live chain balance/gas checks before signing. The SDK repeats P1 with the returned Credential. |
| Inbound collection | **I1** Payee SDK calls Gateway → **I2** Payee route/Price/Challenge → **I3** external verify/settle → **I4** Provider chain call → **I5** domain settlement commit. |
| Recovery | **R1** claim durable recovery work → **R2** independently observe chain evidence → **D1** persist the outcome. P6 and R2 share the RPC connector. |
| Platform / operations | **B1** bootstrap/configure dependencies → **B2** project Secrets into the runtime (expanded in the physical view). **D1** SQL, **C1** cache, **T1** telemetry, and **O1** private management are independent interactions. **X1** selects an external infrastructure endpoint instead of its bundled instance. |

| Boundary | Responsibilities | Ownership constraints |
|---|---|---|
| Browser / application UI | Payer Transactions, Payee Transactions, Approvals, Balances & Ledger, Clients, Scenarios, Wallets, and runtime health | Financial history is read-only. Approvals and supported configuration operations use the domain APIs. |
| AuthGuard | Identity, sessions, identity and access management, and routing authorization | Envoy Gateway routes the UI and all registered protected APIs. AgentVault consumes trusted principal context. |
| Business applications | Payer SDKs in outbound clients; Payee SDKs in resource handlers | SDKs are embedded libraries, not additional AgentVault microservices. Applications own delivery and task state. |
| AgentVault | Payer, Payee, shared domain, protocol adapters, and recovery jobs | Domain logic and SQL transactions enforce payment policy. Only Payer opens wallet custody. |
| External settlement | x402 Facilitators, MPP settlement providers, and configured chain nodes | They remain outside AgentVault and supply independently verified settlement evidence. |
| Infrastructure | Authoritative SQL, optional cache, Secret projection, and observability | Cache and telemetry cannot authorize funds. AuthGuard retains its own identity storage. |

Gateway forwards the original request with signed AuthGuard context or an opaque scope token after HTTP access checks. AgentVault verifies signed context locally; an opaque token can be resolved through AuthGuard's internal scope service. That internal integration and telemetry transport are separate from the HTTP-only business SDK contract. The diagrams deliberately show just Gateway and one coarse-grained AuthGuard component.

AuthGuard may be explicitly disabled for an isolated trusted network. This opens the HTTP identity boundary; Scenario ownership, Policy, approval, wallet, budget, and settlement constraints remain active.

## 2. Implementation layers

| Layer / source | Role |
|---|---|
| [`web/`](../../web/) | React dashboard using same-origin APIs; no browser Bearer-token storage. |
| [`src/sdk/`](../../src/sdk/) | Rust, Go, Python, TypeScript, and Java HTTP SDKs; interceptors, middleware, and HTTP/MCP/A2A payment bindings. |
| [`src/payer/`](../../src/payer/) | Routes and handlers for client management, authorization, approval, signing, wallet import, balances, and outbound evidence. |
| [`src/payee/`](../../src/payee/) | Routes and handlers for Price resolution, Challenge issuance, Credential validation, external settlement, and recovery. |
| [`src/protocol/`](../../src/protocol/) | Direction-scoped registries, x402/MPP wire semantics, constrained signing plans, chain RPC, and receipt observation. |
| [`src/core/`](../../src/core/) | Application config/context, shared domain models and ports, SQL repositories, cache integration, resource import, management, and APM. |
| [`src/common/`](../../src/common/) | Independent framework crates: lifecycle/context, errors, macros, telemetry, audit logging, and profiling. |
| [`src/cmd/`](../../src/cmd/) | CLI, role selection, dependency composition, and runtime job lifecycle. |

Payer and Payee do not depend on each other. They share Core capabilities and use Protocol adapters. Framework crates do not depend on payment domains. `ApplicationContext` registers providers through `IProvider`; the process owns initialization, request draining, and shutdown. Cache closes before SQL after active work drains.

Domain repositories compose generic PostgreSQL/SQLite bases. Entity macros supply column metadata and parameter binding; SQLx decodes rows. Financial aggregate transactions retain explicit locks and conditional writes rather than becoming unrelated CRUD calls.

## 3. Resources, authorization, and budgets

The ownership graph is **Client → Scenario → Policy → Wallet membership**. A Client owns many Scenarios; a Scenario owns many Policies; a Policy authorizes one or more Wallets. Explicit membership permits shared wallets.

| Resource | Meaning and invariant |
|---|---|
| Client | Registered application; `auth_principal` binds it to a trusted principal. |
| Scenario | Payment isolation boundary belonging to one Client; registration alone grants no spending authority. |
| Policy | Protocol/network/asset/origin/recipient restrictions, directional budgets, and approval rules. |
| Wallet | Managed externally owned account or external receiving address; shared wallets retain aggregate hard limits. |
| Price | Authoritative Payee quote bound to Scenario, inbound Policy, and eligible receiving Wallet. |
| Intent | Durable payment state, selected authorization facts, reservations, and recovery progress. |
| Approval | Independent administrator decision bound to an existing Intent and business Run. |
| Ledger | Append-only confirmed financial effects, corrected by explicit reversal entries. |

`run_id` identifies one logical operation, not an authorization identity. `UNIQUE(scenario_id, run_id)` returns the existing result for the same request fingerprint and rejects a changed request. Polling, approval, paid retries, evidence reporting, recovery, and application delivery retain that Run. Retries increase `attempt_no`; traces may change across attempts or restarts without creating another business correlation identifier.

Payer submits `scenario_id + run_id + original challenge`. The trusted principal determines Client ownership. The caller cannot choose Policy, Wallet, amount, asset, network, or recipient for signing; those facts come from the Challenge and persisted configuration.

Eligible Policy–Wallet memberships form a bounded pool. Selection separates direct authorization, approval-required, and rejected candidates, then prefers lower daily spent-plus-reserved usage with a deterministic request-hash tie-break. Matching Policies are alternatives, not intersecting restrictions. SQL locks recheck membership, key version, Policy snapshot, and both Scenario and shared-Wallet budgets. Once an Intent exists, retries do not silently switch Wallets. Authorization uses SQL membership/existence queries; catalog pagination serves presentation only.

Inbound and outbound limits are independent within the Policy budget unit. Inbound limits constrain inventory or business exposure. One-use approval grants add bounded headroom to one Intent; they neither rewrite long-term Policy nor bypass Wallet hard limits. Requesters cannot approve their own payments. Grant claims and recovery material bind Intent, request hash, amount, approver, and expiry, and are consumed atomically during reservation. Approval JWTs are internal Intent-bound proofs, not login tokens; durable state retains claim hashes and protected recovery material.

## 4. Payment and recovery flows

### Outbound: authorize and pay

1. The Payer SDK receives a payment Challenge from an HTTP, MCP, or supported A2A resource.
2. It retains one `run_id` and submits the original Challenge and Scenario to AgentVault.
3. Payer validates ownership and Challenge semantics, selects an eligible Wallet, and reserves directional budget in SQL. If needed, it persists Approval and waits for a permitted independent decision.
4. Before each signature, Payer rechecks lifecycle, authorization, key version, live chain balance, and gas. Protocol adapters create Intent-bound signing plans; arbitrary caller-provided digests are rejected.
5. The SDK retries the same resource operation with the Credential and reports the protocol payment response.
6. Trusted Provider/RPC evidence confirms transfer. Intent, Event, Ledger, and Outbox changes commit atomically. A signature or merchant-supplied receipt alone is not settlement.

### Inbound: collect before delivery

1. A resource handler embeds the Payee SDK and resolves authoritative Price.
2. Payee issues a unique expiring Challenge bound to Scenario, resource, amount, asset, and recipient, reserving inbound capacity.
3. Credential semantics and replay are checked; Payee calls the configured external Facilitator/Provider to verify and settle.
4. Only evidence matching network, token, decimals, amount, recipient, transaction effect, and confirmations permits durable settlement and Receipt.
5. The application releases the resource using its existing `run_id` and task/delivery store. Delivery failure after payment retries delivery, not charging.

Credential fingerprints are unique within each payment direction and cannot be redeemed against another resource or request. A deployment serving both sides can record one outbound and one inbound fact for the same external payment, preserving each wallet's accounting perspective.

External settlement and local SQL cannot share an atomic commit. Durable recovery evidence, idempotent provider calls, SQL leases, and Reconciliation converge uncertain outcomes. Recovery reads exact `(intent_id, event_type, attempt_no)` evidence, not a paginated audit view. SQL Outbox records deduplicate by destination, event type, and `run_id`. Each process has a unique lease owner; recovery workers compete for SQL leases, not broker subscriptions. No business message broker is required. The default CLI runs Reconciliation, approval expiry, and balance synchronization; it does not start a push-notification dispatcher. Outbox delivery is an explicit extension through `PaymentEventDispatcher` and `PaymentEventSink`; SDKs and administrators can query payment/approval state over HTTP.

## 5. Persistence and financial consistency

PostgreSQL or SQLite is the payment authority. The [migrations](../../migrations/) initialize canonical schema and version history transactionally. Startup neither replays initialization nor accepts an unknown version chain.

| Tables | Purpose |
|---|---|
| `pay_client`, `pay_scenario`, `pay_policy`, `pay_price` | Identity ownership, authorization configuration, and quotes. |
| `pay_wallet`, `wallet_key_version`, `pay_policy_wallet` | Wallet metadata, immutable encrypted key versions, hard limits, and membership. |
| `pay_scenario_approver`, `pay_approval` | Permitted approvers, Intent-bound decisions, and one-use grants. |
| `pay_intent`, `pay_event` | Payment state and append-only operational/recovery evidence. |
| `pay_ledger` | Confirmed effects, deduplication, and exact reversals. |
| `pay_outbox` | Durable delivery/recovery records committed with aggregate state. |
| `pay_wallet_balance` | Non-authoritative chain observations for operations. |
| `pay_schema_version` | Forward-only schema history. |

PostgreSQL uses ordered row locks, conditional updates, and SQL leases. SQLite uses `BEGIN IMMEDIATE` and transactional leases, with one AgentVault writer process. Shared PostgreSQL supports multiple replicas. Redis locks do not establish correctness.

Reservation, grant consumption, transitions, key-version changes, and accounting are explicit domain transactions. DDL constraints enforce static relationships and uniqueness; business triggers and stored procedures do not hide state transitions. Domain ports expose financial append/reversal operations, not arbitrary history updates. Credential isolation and controlled migrations are the operational boundary; arbitrary direct SQL writes are unsupported.

Ledger deduplicates by `(wallet_id, posting_key)`. Trusted `effect_ref` values distinguish principal, gas, refunds, and other effects of a transaction. Historical rows are not edited or deleted. Critical persistence failures fail closed: logs, cache, and client assertions cannot establish funds.

## 6. Protocol and application integration

| Binding | Implemented scope |
|---|---|
| x402 v2 / EVM | Exact token payments and EIP-3009 Intent-bound authorization. |
| x402 v2 / Solana | Supported SPL-token exact-payment profiles with configured chain, fee payer, and receipt verification. |
| MPP / Tempo | `charge` using constrained type-0x76 signing and an external settlement provider. |
| HTTP / MCP | Challenge, Credential, paid retry, and protocol-specific Receipt handling. |
| A2A | Explicitly enabled, non-streaming A2A 0.3 JSON-RPC `message/send`, using each payment protocol's own metadata/extension contract. |

MPP session, split, fee sponsorship, machine-token, and delegated-key modes remain unsupported. Automatic A2A payment does not claim streaming, A2A 1.0, or nested AP2 support. Conflicting or unsupported wire representations are rejected rather than guessed.

Challenges select neither provider endpoints nor chain RPC: both come from operator configuration. Adapters use fixed authorities, bounded responses/timeouts, and constrained redirects. x402 absolute expiry survives approval and retries. Receipt reports are evidence-discovery inputs, not accounting authority. SDKs preserve business authentication, prevent cross-origin Credential forwarding, and remove payment metadata before invoking business handlers.

AgentVault does not deploy business A2A or MCP servers. Integrating applications own these servers and embed Payee SDKs to gate delivery; outbound clients embed Payer SDKs. See the [SDK contract](../../src/sdk/README.md#a2a-automatic-payments) for language adapters and wire examples.

## 7. Custody, configuration, and management surfaces

Managed wallet keys use AES-256-GCM envelope encryption and immutable SQL key versions. `agentvault.key_management` references key-encryption keys (KEKs), not EOA private keys. Only Payer opens custody for constrained signing; Payee uses public wallet metadata. Retirement or compromise does not erase history or permit unsigned work to keep using retired keys.

Browser import supports secp256k1 EOA keys and first obtains a short-lived identity-bound RSA-3072 public-key session. Web Crypto encrypts the 32-byte private key with fixed RSA-OAEP/SHA-256 parameters and a wallet/session-bound label. The server atomically consumes the session, briefly decrypts the key, and delegates to existing KEK-protected SQL custody. The temporary private key is recovery-encrypted in TTL cache, not stored as a durable wallet key. Plaintext is excluded from browser state, URLs, logs, and JSON requests. HTTPS remains necessary for authentic page and public-key distribution.

Dashboard and console share domain ports. Transaction/Event/Ledger history and Client identity bindings are read-only in the UI; controlled resource import owns identity/catalog registration. Scenario creation atomically writes Scenario, initial Policy, and selected Wallet membership. Wallet UI/console supports creation and public metadata reads, not private-key export or arbitrary key lifecycle operations. Price writes check revisions, preserve immutable scope/creation identity, and cannot delete referenced history.

Configuration lives in [`src/core/src/config/`](../../src/core/src/config/): defaults < native YAML < `AGENTVAULT__...` environment overrides. Generic Serde-tree traversal handles nested maps/arrays; `validation.rs` checks cross-field constraints. Secret references keep material outside serialized config. Framework roots are `server`, `mgmt`, `logging`, `storage`, `cache`, and `secrets`; business configuration belongs under `agentvault`, with independently enabled Payer/Payee profiles.

## 8. Physical deployment and operations

![AgentVault physical architecture: shared Gateway, Web deployment, replicated combined-role backend, private management, and independent infrastructure](assets/agentvault—physical-architecture.svg)

The diagram depicts default Helm deployment: separate Web replicas and replicated **combined-role** backends. Payer and Payee are module/process roles, not two mandatory deployments. `agentvault serve --role payer` and `--role payee` support independent processes with the same HTTP contract. Compose defaults to one combined backend.

| Endpoint / component | Deployment behavior |
|---|---|
| Envoy Gateway | Shared routing for UI, protected `/api/v1/*`, and registered application routes. Configure production edge TLS; Compose's development edge publishes HTTP `:8080`. |
| Web / Nginx | Production static image [`web/Dockerfile`](../../web/Dockerfile), internal port `8080`; APIs remain same-origin through Gateway. |
| Backend | Business and Admin HTTP routers on `8080`; management `9091` is private and hosts no catalog/payment write routes. |
| PostgreSQL / Redis / Jaeger | Independent default-on dependencies from [`deploy/helm/`](../../deploy/helm/). AuthGuard owns its database/cache separately. |
| Secret projection | Kubernetes Secret or configured GCP/AWS/Vault integration; bootstrap precedes installation. Payment keys are not Helm values or SDK configuration. |
| Providers / chain nodes | External operator-configured dependencies, not embedded AgentVault containers. Local chain nodes are disposable use-case fixtures. |

Helm uses `postgres.enabled`, `redis.enabled`, and `jaeger.enabled`; Compose uses the `COMPOSE_PROFILES` defaults in `.env.example`. Disable bundled instances and supply external SQL/cache/collector endpoints when reusing infrastructure. SQLite with a local durable volume is a single-process alternative, not a shared multi-replica database.

`ApmComponent` starts health, metrics, tracing, and configured diagnostics. `/healthz` checks process liveness; `/readyz` probes authoritative SQL within a bounded timeout and returns 503 on failure. Cache failure is observed and can degrade acceleration, but neither rewrites funds nor alone marks SQL-ready instances unavailable.

Wallet Balance Sync stores periodic chain observations for operations. Signing still checks live balance/gas; snapshots cannot replace settlement evidence or Ledger. Structured logs and OpenTelemetry propagate trace/span context through SDK, Vault, Provider, and RPC calls without imposing business constraints. External collectors can retain traces; bundled Jaeger uses bounded in-memory storage.

## 9. Verification and related documentation

The [MarketPulse use case](../../use-cases/marketpulse-feed-subscription/README.md) covers five Payer and five Payee SDKs: 25 isolated full HTTP combinations or four Rust/Go combinations. Each case cross-checks selected wallets, SQL Intent/Ledger/Receipt, chain balances, settlement effects, gas, and trace parentage. Separate Solana, Tempo, and A2A scenarios establish their own evidence, not proof of every profile through a single scenario.

Docker and Kubernetes CI also run `live-wallet`: Chromium loads the **production Web container**, calls live APIs through Nginx without browser API interception, and checks payment views, filters, balances, approvals, Client ownership, encrypted wallet custody, and atomic Scenario creation, with screenshots and SQL assertions. The optional `web` scenario uses immutable-report-derived fixtures for larger presentation/pagination checks; it is not described as a live production database test.

- [SDKs and A2A bindings](../../src/sdk/README.md)
- [Docker deployment](../../deploy/docker/README.md)
- [Helm deployment and Secret bootstrap](../../deploy/helm/agentvault/README.md)
- [End-to-end scenarios and evidence](../../use-cases/marketpulse-feed-subscription/README.md)

Diagram footers define their abbreviations. In this text: EOA = externally owned account; JWT = JSON Web Token; CRUD = create/read/update/delete; SQLx = the Rust SQL toolkit; TTL = time to live. Wire examples and operational commands remain in their owning documents.
