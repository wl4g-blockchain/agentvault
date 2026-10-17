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
  request intent-bound signing over MQTT through their language SDK, retry the
  identical MCP operation with the returned credential, submit the official
  payment response, and durably store the result. They configure only a
  Scenario—never a Wallet, Policy, or projected payment field. AgentVault
  selects a signable EOA from the Scenario's two-member Policy wallet pool;
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
       └─ AgentVault SDK → MQTT authorize         │
            └─ AgentVault: Client/Scenario        │
               → Policy/Wallet/balance/budget     │
               ├─ authorized → EOA sign           │
               └─ approval required → MQTT ───────┼→ QuantStream Admin UI
                    → one-time internal JWT grant  │
                    → atomic consume/recheck/sign  │
       ← credential; retry identical MCP request  │
                         MarketPulse payee ←───────┘
                           → official Facilitator → Anvil/TestUSDC
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

The provisioning manifest expresses `Client 1:N Scenario`, `Scenario 1:N
Policy`, and `Policy 1:N Wallet membership`. A Wallet may appear in more than
one Policy so multiple Scenarios can share it. Among equally authorized
candidates AgentVault prefers the lowest daily usage and uses a stable hash to
break ties; Wallet-wide hard limits and deterministic transactional locks remain
authoritative.

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
- `infra`: PostgreSQL, EMQX, and Jaeger.
- `apps`: AgentVault, QuantStream Admin UI, and the read-only auditor.
- `payee-{go,rust,python,java,typescript}`: exactly one payee backend.
- `ingestor-{go,rust,python,java,typescript}`: exactly one payer backend.

The asserted payment is x402 v2 / EVM / EIP-3009 against compiled TestUSDC.
Solana is a real validator health dependency but no Solana payment is claimed.
No mock RPC, broker, or Facilitator is used.
MarketPulse also emits AgentVault's `payment-expires-at` HTTP profile field as
canonical Unix seconds. Every approval poll reuses that absolute value, so the
same official x402 body always maps to the same durable Challenge Hash.

## Project layout

```text
marketpulse-feed-subscription/e2e/
├── deploy/
│   ├── anvil/  solana/  facilitator/
│   ├── mock-payee-marketpulse-{golang,rust,python,java,typescript}-service/
│   │   └── {main/Application,server,route/feed,handler/feed,storage/feed}
│   ├── mock-payer-quantstream-ingest-ui/src/
│   ├── mock-payer-quantstream-ingest-service-golang/
│   │   ├── main.go
│   │   └── pkg/{route,handler,job,agent}/
│   ├── mock-payer-quantstream-ingest-service-rust/
│   │   └── src/{lib.rs,main.rs,route/,handler/,job/,agent/}
│   ├── mock-payer-quantstream-ingest-service-python/
│   │   └── app/{main.py,route/,handler/,job/,agent/}
│   ├── mock-payer-quantstream-ingest-service-java/
│   │   └── src/io/agentvault/e2e/{Application.java,route/,handler/,job/,agent/}
│   ├── mock-payer-quantstream-ingest-service-typescript/
│   │   └── src/{main.ts,route/,handler/,job/,agent/}
│   └── */tests/           # source-path-mirrored unit tests
├── common/contract/       # provisioning and MQTT wire contracts
├── verifier/              # structure and financial/business assertions
├── reports/               # generated layered reports
├── docker-compose.yaml
└── runner.py              # standard-library layered orchestrator
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

python3 runner.py --list
python3 runner.py -s structure
python3 runner.py -s infrastructure
python3 runner.py -s payer
python3 runner.py -s payee
python3 runner.py -s full --payer-language rust --payee-language rust
python3 runner.py -s full --payer-language go --payee-language java
python3 runner.py -s matrix
python3 runner.py cleanup
```

The shell entry point remains available as `deploy/e2e.sh`; the Python runner
adds structure validation and writes immutable evidence under
`reports/<yyyyMMddHHmmss>/<module>/` plus that run's `summary.md`. Deployments
are retained by default for manual inspection. Use `--cleanup-after-run` for an
explicit teardown after a run, or `runner.py cleanup` later.

## Full-flow acceptance evidence

A payer/payee language-pair run fails unless it proves all of the following.
`matrix` executes all 25 language pairs sequentially:

1. free MCP delivery and a genuine x402 v2 402 for the paid tool;
2. two stable logical operations, including a delivery retry without a second
   authorization or a changed credential;
3. MQTT approval notification, UI decision, one consumed exact-amount grant,
   and automatic continuation of the same Intent;
4. two OUTBOUND and two INBOUND settled Intents, four confirmed Ledger payment
   facts, bound receipts, and settlement/reconciliation Events;
5. two durable MarketPulse deliveries and two durable QuantStream ingestion
   rows; and
6. an actual 2 USDC TestUSDC balance transfer from payer to payee.

Provider fee/refund/deposit/transfer/reversal matrices, MPP/Tempo settlement,
and Solana payment are not overstated here; they require their own provider
fixtures and acceptance scenarios.
