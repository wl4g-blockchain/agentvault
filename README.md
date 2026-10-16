# AgentVault

Enterprise payment infrastructure for autonomous AI agents.

AgentVault is a Rust service that mediates both sides of machine payments:

- Payer / OUTBOUND: authorize a paid HTTP/MCP request, reserve budget, obtain approval when needed, sign an intent-bound credential, and reconcile settlement.
- Payee / INBOUND: resolve a fixed Offer, issue a 402 challenge, verify and settle an incoming credential through an external provider, then publish a trusted receipt.

Private keys and all financial state are stored only in PostgreSQL or SQLite. Keys use versioned AES-256-GCM envelope encryption with full UUID v4 nonces; Ledger rows are append-only. Memory/Redis caches contain only reconstructible state and never decide budgets, approvals, idempotency, settlement, or balances.

## Architecture

```text
Flowgent / Sigbot / third-party agents
        │ SDK: Rust / Go / Python / TypeScript / Java
        ▼
AuthGuard + Envoy (default, optional) ── HTTP identity and IAM
        ▼
AgentVault ── Scenario / Policy / Approval / Signer / Settlement / Ledger
        ▼
External x402 Facilitator / MPP Provider / fixed RPC endpoints
```

AgentVault does not implement login, OAuth2/OIDC, sessions, or an IAM policy engine. The standard Docker and Helm deployments put every HTTP payment and Admin API behind AuthGuard. An operator may explicitly disable that integration, in which case HTTP is open by design. MQTT is independent and uses broker authentication, deny-by-default topic ACLs, and durable Client/Scenario mappings. Approval decisions use a separately authenticated administrator topic whose client and approver principal are verified against the SQL Scenario/Approver mapping; payloads cannot claim either identity.

The resource hierarchy is:

```text
Client 1 ── N Scenario 1 ── N Policy 1 ── N Wallet
                                      N ── 1 shared Wallet
```

A wallet may be shared by multiple policies because enterprise scenarios can use one treasury wallet. Shared wallets require aggregate hard limits. Every policy has independent inbound and outbound per-payment, daily, approval-threshold, and approval-overage controls.

Read the [architecture white paper](docs/index.md) for flows, invariants, table relationships, security boundaries, and deployment topology.

## Repository layout

```text
src/          Rust service: apm, cache, cmd, config, controller, handler,
              model, protocol, storage, utils
migrations/   matching PostgreSQL and SQLite 001_init DDL/DML
src/adapters/ Rust, Go, Python, TypeScript, and Java SDKs plus shared transport proto
web/          AgentVault operations dashboard
deploy/       Grafana, Docker, and Helm deployment assets
use-cases/    real MarketPulse/QuantStream multi-process payment E2E
docs/         one canonical architecture white paper
```

There is no public API-description directory. SDK source DTOs and cross-language contract tests are the supported client boundary.

## Build and test

Rust builds are pinned by `rust-toolchain.toml` and `Cargo.lock`.

```bash
make fmt-check
make lint
make test
make build
```

The Makefile checks `target/` before every build and runs `cargo clean` if it exceeds 10 GiB.

SDK checks:

```bash
(cd src/adapters/rust && cargo test --locked)
(cd src/adapters/golang && go test ./...)
(cd src/adapters/python && PYTHONPATH=. python3 -m unittest -v $(find tests -type f -name '*_test.py' | sort | sed 's#/#.#g;s#\.py$##'))
(cd src/adapters/typescript && npm ci && npm test && npm run build)
(cd src/adapters/java && mvn test)
```

Dashboard checks:

```bash
(cd web && npm ci && npm test && npm run build)
```

## Deployment

The default Docker topology includes AuthGuard and Envoy:

```bash
cp deploy/docker/.env.example deploy/docker/.env
${EDITOR:-vi} deploy/docker/.env
docker compose --env-file deploy/docker/.env \
  -f deploy/docker/docker-compose.yaml up -d
```

For an intentionally open trusted network, use `docker-compose.open.yaml`. Helm provides the same default-on AuthGuard dependency under `deploy/helm/agentvault/`.

The checked-in service configuration is safe by default: both `agentvault.payer.enabled` and `agentvault.payee.enabled` are false. Production must provide SQL, Wallet KEKs, direction-specific adapters/Providers, the shared chain registry, and optional authenticated MQTT settings through operator-owned configuration and secret references.

## Real E2E

`use-cases/marketpulse-feed-subscription/e2e/` builds two business systems:

- MarketPulse Payee: a free previous-day MCP news tool and a paid real-time tool.
- QuantStream Payer: equivalent Rust, Go, Python, TypeScript, and Java ingestion services plus an administrator approval UI.

The test uses PostgreSQL, authenticated EMQX ACLs, the official x402 Facilitator, Anvil/TestUSDC, a real Solana node health dependency, AgentVault, the Payee, and one SDK process. It verifies 402 retry, MQTT signing, daily-limit approval, one-time grant consumption, EVM settlement, receipt, Ledger, and delivery recovery.

```bash
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s structure
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s infrastructure
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s payer
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s payee
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s full \
  --payer-language rust --payee-language rust
python3 use-cases/marketpulse-feed-subscription/e2e/runner.py -s matrix
```

Solana payment is not asserted by this E2E; the Solana node is a real dependency and health boundary. The financial flow currently asserted end-to-end is x402 exact on EVM.

## License

Apache-2.0. See [LICENSE](LICENSE).
