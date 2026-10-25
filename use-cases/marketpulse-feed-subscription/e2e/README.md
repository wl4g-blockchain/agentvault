# MarketPulse Feed Subscription E2E

This is a from-zero, process-level acceptance project for AgentVault. It models
two real business systems rather than a unit-test fixture:

- **MarketPulse** is a payee data platform implemented as five equivalent
  Rust/Axum, Go/Gin, Python/FastAPI, Java/Spring Boot, and
  TypeScript/Express services. Each service owns the same
  `main/Application → server → SDK PayeeMiddleware + MCP → handler/feed →
  storage/feed` boundary and initializes exactly 100 realistic SQLite news
  records. `route/feed` (Java: `FeedController`) contains only operational
  `/healthz` and `/metrics` endpoints; the server owns the paid MCP listener.
  Its MCP endpoint exposes
  `marketpulse.yesterday_news` for free and `marketpulse.realtime_news` for one
  USDC. The paid tool resolves a server-owned Offer, emits x402 v2 402, asks
  AgentVault to verify and settle through the external official Facilitator,
  and delivers data only after confirmed settlement.
- **QuantStream** is a payer quantitative-ingestion platform. Five equivalent
  Go, Rust, Python, Java, and TypeScript services poll both tools, detect 402,
  request intent-bound signing over the case-selected MQTT or gRPC transport
  through their language SDK, retry the
  identical MCP operation with the returned credential, submit the official
  payment response, and durably store the result. They configure only a
  language-specific Scenario—never a Wallet, Policy, or projected payment field.
  Each Payer owns an independent Client, Scenario, Policy, MQTT identity and two
  funded EOAs; AgentVault selects a signable EOA only from that two-member pool;
  the services never hold an EOA key. The two paid runs must settle from two
  distinct Wallets, proving selection rather than merely hiding a fixed ID.

The Node admin UI is part of QuantStream, not AgentVault. It consumes durable
approval notifications, reconciles pending approvals after reconnect, displays
the exact amount/intent, and publishes an administrator decision. Its local UI
token is a business-level second check, independent of HTTP IAM.

## Payment topology

```text
QuantStream service (Go/Rust/Python/Java/TS)
  ├─ free MCP call ───────────────────────────────┐
  └─ paid MCP call → 402                          │
       └─ AgentVault SDK → MQTT / gRPC authorize  │
            └─ AgentVault: Client/Scenario        │
               → Policy/Wallet/balance/budget     │
               ├─ authorized → EOA sign           │
               └─ approval required → MQTT ───────┼→ QuantStream Admin UI
                    → one-time internal JWT grant  │
                    → atomic consume/recheck/sign  │
       ← credential; retry identical MCP request  │
                         MarketPulse payee ←───────┘
                           → official Facilitator → Anvil/E2EUSDC
                           → receipt + inbound/outbound Ledger
```

The direct Docker E2E sets `server.authguard.enabled=false` because it tests the
payment and MQTT boundary, not the IAM product. This makes HTTP intentionally
open inside a loopback-only disposable namespace. Standard AgentVault Docker
and Helm deployments enable AuthGuard/Envoy by default. MQTT never traverses
AuthGuard; EMQX credentials/deny-by-default ACLs and AgentVault's durable
topic identity mappings form its separate identity boundary. Workload commands
derive `client_id` from their topic. Approval decisions use a distinct
`clients/{client}/approvers/{principal}/decisions` topic: EMQX binds the exact
topic to the administrator account, and AgentVault independently verifies the
topic-derived principal in the Scenario Approver table. The message body cannot
override either identity.

## Limits and approval behavior

The canonical console-resource fixture expresses `Client 1:N Scenario`,
`Scenario 1:N Policy`, and `Policy 1:N Wallet membership`. Production may share a Wallet
across Policies, but this matrix deliberately provisions five isolated Payer
identities and ten distinct EOAs so cross-SDK leakage is observable. Among
equally authorized candidates AgentVault prefers the lowest daily usage and
uses a stable hash to break ties; Wallet-wide hard limits and deterministic
transactional locks remain authoritative.

- QuantStream OUTBOUND normal daily allowance: 1 USDC.
- One additional payment enters `PendingApproval`.
- One-time approval overage ceiling: 50 USDC.
- Each payer Wallet hard daily ceiling: 51 USDC.
- The two settled OUTBOUND Intents must reference both pool members exactly
  once; each EOA must lose exactly 1 USDC on Anvil.
- MarketPulse INBOUND sales/receipt ceiling: 10 USDC per day, preventing
  overselling even though funds flow toward the payee.

An approval grant is bound to intent ID, immutable request hash, amount,
approver, expiry, and nonce. The JWT is internal only; the Approval row stores
its `jti`, claims hash, and versioned AEAD ciphertext. Consumption, budget
reservation, balance recheck, and transition to signing are one SQL transaction.

## Real dependencies and profiles

All dependency definitions are under `deploy/`, copied and adapted from the
Flowgent Anvil, Solana, and Facilitator deployments:

- `payer`: Anvil and Solana nodes; no Facilitator container.
- `payee`: Anvil, Solana, and official x402 Facilitator.
- `tempo`: the digest-pinned official Tempo localnet used only by the explicit
  MPP `charge` gate.
- `infra`: PostgreSQL, EMQX, and Jaeger.
- `apps`: AgentVault, QuantStream Admin UI, and the read-only auditor.
- `payee-{go,rust,python,java,typescript}`: exactly one payee backend.
- `ingestor-{go,rust,python,java,typescript}`: exactly one payer backend.

The five-language business matrix asserts x402 v2 / EVM / EIP-3009 against
compiled E2EUSDC. The separate `mpp-tempo` scenario asserts a Vault-signed
Tempo type-0x76 `charge`, external HTTP verify/settle, lost-response recovery,
single broadcast, receipt binding, and pathUSD balance conservation including
token-denominated gas. The explicit `x402-solana` scenario creates a real SPL
mint and associated token accounts on the pinned local validator, then runs the
pinned upstream x402 v2 client and facilitator through verify and one settle.
It asserts exact SPL deltas, aggregate token conservation, unchanged payer
lamports, and the facilitator's exact confirmed transaction fee. No mock RPC,
broker, or Facilitator is used.
MarketPulse also emits AgentVault's `payment-expires-at` HTTP profile field as
canonical Unix seconds. Every approval poll reuses that absolute value, so the
same official x402 body always maps to the same durable Challenge Hash.

## Project layout

```text
marketpulse-feed-subscription/e2e/
├── deploy/
│   ├── deployer/          # Docker/Kubernetes lifecycle and scoped cleanup
│   ├── namespaces.yaml    # cluster-level E2E namespace manifest
│   ├── agentvault-helm-shared/
│   ├── mock-payee-marketpulse-config/
│   ├── mock-payer-quantstream-config/
│   ├── anvil/  solana/  facilitator/
│   ├── mock-payee-marketpulse-{golang,rust,python,java,typescript}-service/
│   │   └── {main/Application,server,route/feed,handler/feed,storage/feed}
│   ├── mock-payer-quantstream-ui/src/
│   ├── mock-payer-quantstream-service-golang/
│   │   ├── main.go
│   │   └── pkg/{route,handler,job,agent}/
│   ├── mock-payer-quantstream-service-rust/
│   │   └── src/{lib.rs,main.rs,route/,handler/,job/,agent/}
│   ├── mock-payer-quantstream-service-python/
│   │   └── app/{main.py,route/,handler/,job/,agent/}
│   ├── mock-payer-quantstream-service-java/
│   │   └── src/io/agentvault/e2e/{Application.java,route/,handler/,job/,agent/}
│   ├── mock-payer-quantstream-service-typescript/
│   │   └── src/{main.ts,route/,handler/,job/,agent/}
│   └── */tests/           # source-path-mirrored unit tests
├── common/                # Python-only case context and shared utilities
├── config/                # AgentVault runtime and initialization configuration
│   ├── agentvault/
│   │   ├── values.yaml    # the single Helm/Kubernetes values source
│   │   └── init.import.yaml # canonical console-resource import
├── verifier/              # structure and financial/business assertions
├── reports/               # generated layered reports
├── docker-compose.yaml
└── runner.py              # layered orchestrator with dependencies in requirements.txt
```

The five payer services intentionally keep the same composition: thin
`main/Application`, operational-only `route/ingestion → handler/ingestion`, and
the independent scheduled path `job/ingestion_job → agent/mcp`.
`MCPHelper` uses mark3labs/mcp-go, rmcp, the official Python MCP package, the
official Java MCP SDK, or the official TypeScript MCP SDK respectively. Each Job
registers two independent cron functions: the daily free yesterday-news tool
and the higher-frequency paid realtime-news tool. The MCP service owns the two
external tool definitions and integrates the language SDK's Payer Middleware;
framework glue never implements payment logic.

## Run layered acceptance

```bash
cd use-cases/marketpulse-feed-subscription/e2e
python3 -m pip install --requirement requirements.txt

python3 runner.py --list
python3 runner.py -s structure
python3 runner.py -s infrastructure
python3 runner.py -s payer
python3 runner.py -s payee
python3 runner.py -s full --payer-language rust --payee-language rust
python3 runner.py -s full --payer-language go --payee-language java
python3 runner.py -s mpp-tempo
python3 runner.py -s x402-solana
python3 runner.py -s matrix --matrix-workers 2 # default simple: Rust/Go 2×2×2 = 8
python3 runner.py -s matrix --e2e-sdk-scope full --matrix-workers 2 # 5×5×2 = 50
python3 runner.py -s matrix --e2e-sdk-scope full --resume-run <yyyyMMddHHmmss> --skip-build --cleanup-after-run
python3 runner.py -s web --e2e-sdk-scope full --matrix-evidence-run <completed-matrix-run>
KUBECONFIG=/etc/rancher/k3s/k3s.yaml python3 runner.py --deployer kubernetes -s full --payer-language rust --payee-language rust
KUBECONFIG=/etc/rancher/k3s/k3s.yaml python3 runner.py --deployer kubernetes -s matrix --matrix-workers 1
python3 runner.py cleanup
python3 runner.py --cleanup
```

`runner.py` is the only outer orchestration entry. It owns build, deployment,
health checks, scenario selection, diagnostics and cleanup, and writes immutable evidence under
`reports/<yyyyMMddHHmmss>/<module>/` plus that run's `summary.md`. Deployments
are retained by default for manual inspection. Use `--cleanup-after-run` for an
explicit teardown after a run, or `runner.py cleanup` later.
The `cleanup` command and `--cleanup` flag are equivalent. They remove the
shared and worker Compose containers, project-labelled named/anonymous volumes,
allowlisted local AgentVault/MarketPulse/QuantStream `*:local` images, and Helm
releases inside manifest-declared, labelled `e2e-agentvault-*` namespaces. Helm
gets a bounded graceful uninstall first. Only if that fails are release-labelled
resource finalizers cleared, the uninstall retried without hooks, and stranded
Pods force-deleted. Namespaces and bootstrap objects are retained for a clean
redeploy. Cleanup never prunes external base images, registries, unrelated
volumes, global builder caches, or resources outside those namespaces.

`--cleanup-after-run` remains intentionally narrower. The Docker backend removes
only current Compose containers and volumes; the Kubernetes backend uninstalls
only the current E2E Helm releases. Both preserve locally built images. Docker
post-run cleanup never touches a separately deployed Kubernetes review environment.

All ten language service images use separate builder and runtime stages. Rust
and Java copy only their release/runtime artifacts into a non-root distroless
base, Python copies only installed packages into a non-root distroless Python
3.11 runtime, TypeScript copies pruned production dependencies into a non-root
Node 22 distroless runtime without a duplicate ownership layer, and Go retains
its static binary plus Alpine runtime.

## Kubernetes namespace contract

`runner.py` exposes `--deployer docker|kubernetes`; Docker remains the CI
default. The Kubernetes backend owns local-image build/import, Helm lifecycle,
port forwards, payer Jobs, approval, and PostgreSQL/on-chain evidence. Matrix
cases run serially on the shared cluster to prevent cross-case state leakage.
The component Helm charts enforce a deployment boundary for Kubernetes consumers:
every rendered Deployment, Job, ConfigMap, and Service carries an explicit
`e2e-agentvault-*` namespace sourced from a fixed Chart annotation. A Chart
also rejects installation when the Helm release namespace differs, so Helm's
release Secret cannot leak into `default` or another namespace. Bootstrap
the namespace set before installing any E2E release:

```bash
export KUBECONFIG=/etc/rancher/k3s/k3s.yaml
kubectl apply -f deploy/namespaces.yaml
```

| Workload group | Namespace |
| --- | --- |
| AgentVault | `e2e-agentvault-system` |
| standalone AuthGuard and its middleware | `e2e-agentvault-authguard-system` |
| PostgreSQL, EMQX, Jaeger | `e2e-agentvault-infra-system` |
| Anvil, Tempo, EVM fixture, Facilitator, Solana | `e2e-agentvault-payment-system` |
| all QuantStream mock payer services and UI | `e2e-agentvault-mock-payer` |
| all MarketPulse mock payee services | `e2e-agentvault-mock-payee` |

Install the production AgentVault chart into `e2e-agentvault-system`. Component
charts must likewise be installed with `--namespace` set to the namespace in
the table; the render fails closed otherwise. The AgentVault chart's
umbrella AuthGuard dependency remains disabled for this direct payment E2E; an
IAM-enabled variant must install AuthGuard as a standalone release in
`e2e-agentvault-authguard-system`, rather than co-locating it with AgentVault.
The namespace is the only E2E naming boundary: workload, Pod, Service, and
ConfigMap names use their production-style component names without an `e2e-`
or `e2e-agentvault-` prefix. The structure gate rejects missing namespaces,
wrong component ownership, prefixed component names, and templates that stop
applying the explicit namespace.

The `web` scenario follows the AuthGuard browser-verifier pattern: it first
validates that `s21`, `s22`, `s24` and `s30` contain the exact same completed
cases for the requested SDK scope, then maps those immutable facts into same-origin API fixtures for
the production Dashboard bundle. Real headless Chromium asserts and screenshots
ten-row Payer/Payee pagination, full-fact Ledger conservation behind a ten-row
view, a single-column scrolling dropdown with twelve Wallet options, and atomic
Scenario + two-Wallet creation. This is intentionally evidence-backed UI verification; it
does not pretend the isolated per-case databases are one production database.

## Full-flow acceptance evidence

A payer/payee language-pair run fails unless it proves all of the following.
`matrix` defaults to `--e2e-sdk-scope simple`: Rust/Go Payers × Rust/Go Payees ×
2 signing transports = 8 isolated cases. `--e2e-sdk-scope full` executes all
5 × 5 × 2 = 50 cases. Two bounded workers are used by default: one owns all
MQTT cases and one owns all gRPC cases. Each worker has a distinct Compose project, port range, runtime
configuration, database volume and chain state; `--matrix-workers 1` provides a
serial fallback.

Every case writes an identity-bound checkpoint only after all five detailed
verifiers pass. The report also stores an immutable SDK scope/plan. If the host
or runner is interrupted, `--resume-run` validates that plan and all checkpoints,
rejects scope mismatch/corrupt evidence, and executes only the remaining cases.

1. free MCP delivery and a genuine x402 v2 402 for the paid tool;
2. two stable logical operations, including a delivery retry without a second
   authorization or a changed credential;
3. the selected MQTT/gRPC signing request reaches AgentVault, while MQTT
   approval notification, UI decision, one consumed exact-amount grant, and
   automatic continuation of the same Intent;
4. two OUTBOUND and two INBOUND settled Intents, four confirmed Ledger payment
   facts, bound receipts, and settlement/reconciliation Events;
5. two durable MarketPulse deliveries and two durable QuantStream ingestion
   rows; and
6. the active Payer's two EOAs each lose exactly 1 USDC, the other eight Payer
   EOAs remain byte-for-byte unchanged, the gateway/payee gains exactly 2 USDC,
   total token balance is conserved, both Anvil receipts and exact facilitator
   native gas spend match the four Ledger postings, and all ten AgentVault SQL
   snapshots converge to chain state.

`mpp-tempo` and `x402-solana` are deliberately independent of that EVM SDK
matrix. The former proves the implemented MPP Tempo `charge` core against a
real node and a local HTTP settlement service backed by the official `mpp`
Rust server/Tempo provider primitives. The supported direct-charge flow
validates without mutation and broadcasts exactly once through Tempo RPC; it
does not invent an x402-style facilitator or a duplicate merchant contract. The latter proves the pinned
upstream x402 Solana exact client/facilitator and real SPL accounting boundary.
The shared five-SDK fixture separately covers both transports, both protocols,
and both roles. MPP session/split/fee-payer/delegated methods and provider
fee/refund/deposit/transfer/reversal matrices remain unsupported or require
their own acceptance scenarios.
