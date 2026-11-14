# AgentVault

Enterprise payment infrastructure for autonomous AI agents.

AgentVault is a Rust service that mediates both sides of machine payments:

- Payer / OUTBOUND: authorize a paid HTTP/MCP request, reserve budget, obtain approval when needed, sign an intent-bound credential, and reconcile settlement.
- Payee / INBOUND: resolve a fixed Price, issue a 402 challenge, verify and settle an incoming credential through an external provider, then publish a trusted receipt.

Private keys and all financial state are stored only in PostgreSQL or SQLite. Keys use versioned AES-256-GCM envelope encryption with full UUID v4 nonces; Ledger rows are append-only. Memory/Redis caches contain only reconstructible or safely disposable short-lived coordination state and never decide budgets, approvals, idempotency, settlement, or balances.

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

AgentVault does not implement login, OAuth2/OIDC, sessions, or an IAM policy engine. The standard Docker and Helm deployments put every HTTP payment and Admin API behind AuthGuard. An operator may explicitly disable that integration, in which case HTTP is open by design. SDK and browser callers use standard HTTP through that boundary. Approval decisions are HTTP operations checked against durable Scenario/Approver mappings; request bodies cannot claim either identity.

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
src/sdk/ Rust, Go, Python, TypeScript, and Java SDKs plus shared transport proto
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

`make build` builds the release binary and every supported client SDK: Rust,
Go, Python (wheel), TypeScript (JavaScript and declarations), and Java (JAR).
Use `make build-sdks` to omit the server binary, or invoke an individual
SDK target such as `make build-sdk-typescript`.

The Makefile checks both Rust `target/` directories before every build and runs
`cargo clean` if either exceeds 10 GiB.

SDK checks:

```bash
(cd src/sdk/rust && cargo test --locked)
(cd src/sdk/golang && go test ./...)
(cd src/sdk/python && PYTHONPATH=. python3 -m unittest -v $(find tests -type f -name '*_test.py' | sort | sed 's#/#.#g;s#\.py$##'))
(cd src/sdk/typescript && npm ci && npm test && npm run build)
(cd src/sdk/java && mvn test)
```

Dashboard checks:

```bash
(cd web && npm ci && npm test && npm run build)
(cd use-cases/marketpulse-feed-subscription && \
  python3 runner.py -s web --e2e-sdk-scope full \
  --matrix-evidence-run <completed-matrix-run>)
```

## Management console

`agentvault console` opens the interactive administrator console; the same
commands can be executed non-interactively. Catalog creation reuses the
canonical resource importer, lifecycle changes reuse the storage/domain ports,
approval decisions reuse the production approval state machine, and payment,
approval, Ledger, and Event history is read-only.

```bash
agentvault --config etc/agentvault.yaml console import resources/
agentvault --config etc/agentvault.yaml console list client
agentvault --config etc/agentvault.yaml console wallet create wallet.yaml
agentvault --config etc/agentvault.yaml console wallet list
agentvault --config etc/agentvault.yaml console wallet get wallet-id
```

Wallet custody intentionally has a narrower command surface than other
catalog resources: only `create`, `list`, and `get` exist, and both read
commands return public metadata only. There is no console or Dashboard command
to reveal, export, update, rotate, disable, or delete an EOA private key. Once
created, the encrypted key is available only to the runtime signer through the
custody port; neither API serializes key material.

## Deployment

The default Docker topology includes AuthGuard and Envoy:

```bash
cp deploy/docker/.env.example deploy/docker/.env
${EDITOR:-vi} deploy/docker/.env
docker compose --env-file deploy/docker/.env \
  -f deploy/docker/docker-compose.yaml up -d
```

For an intentionally open trusted network, use `docker-compose.open.yaml`. Helm provides the same default-on AuthGuard dependency under `deploy/helm/agentvault/`.

The checked-in service configuration is safe by default: both `agentvault.payer.enabled` and `agentvault.payee.enabled` are false. Production must provide SQL, Wallet KEKs, direction-specific adapters/Providers, the shared chain registry, and HTTP gateway access settings through operator-owned configuration and secret references.

## Real E2E

`use-cases/marketpulse-feed-subscription/` builds two business systems:

- MarketPulse Payee: a free previous-day MCP news tool and a paid real-time tool.
- QuantStream Payer: equivalent Rust, Go, Python, TypeScript, and Java ingestion services plus an administrator approval UI.

The x402 SDK matrix uses PostgreSQL, the official x402 Facilitator, Anvil/E2EUSDC, AgentVault, the Payee, and one SDK process. It verifies 402 retry, HTTP authorization and signing, daily-limit approval, one-time grant consumption, EVM settlement, receipt/Ledger/transaction binding, exact facilitator gas accounting, and delivery recovery. An explicit x402 Solana gate runs the pinned upstream client/facilitator against a real local validator and proves exact SPL and lamport-fee conservation. A separate MPP gate uses the digest-pinned official Tempo localnet and an HTTP settlement boundary backed by the official MPP Rust SDK to verify a Vault-signed type-0x76 charge, verify/settle, lost-response recovery, one broadcast, receipt, and pathUSD conservation including gas.

The five SDK protocol providers use x402 v2 for both HTTP (`PAYMENT-REQUIRED`, with compatible JSON-body parsing and conflict rejection) and MCP JSON-RPC (`structuredContent`/text challenges plus protocol-owned `_meta` credentials and receipts). MPP likewise supports HTTP and MCP through the official paymentauth wire types; a challenged payment header never overwrites an unrelated application Bearer token. Protocol-owned credential metadata is removed before the protected business handler runs.

```bash
python3 use-cases/marketpulse-feed-subscription/runner.py -s structure
python3 use-cases/marketpulse-feed-subscription/runner.py -s infrastructure
python3 use-cases/marketpulse-feed-subscription/runner.py -s payer
python3 use-cases/marketpulse-feed-subscription/runner.py -s payee
python3 use-cases/marketpulse-feed-subscription/runner.py -s full \
  --payer-language rust --payee-language rust
python3 use-cases/marketpulse-feed-subscription/runner.py -s mpp-tempo
python3 use-cases/marketpulse-feed-subscription/runner.py -s x402-solana
python3 use-cases/marketpulse-feed-subscription/runner.py -s matrix # simple: 8 cases
python3 use-cases/marketpulse-feed-subscription/runner.py -s matrix \
  --e2e-sdk-scope full # full: 50 cases
python3 use-cases/marketpulse-feed-subscription/runner.py --cleanup
```

The x402 EVM language matrix, x402 Solana payment gate, and MPP Tempo charge gate produce separate immutable reports so one chain profile cannot mask another.

## License

Apache-2.0. See [LICENSE](LICENSE).

The Rust server is a Cargo workspace with `common`, `protocol`, `payer`, `payee`
and `cmd` crates. Shared configuration is defined in
`src/common/src/config/config.rs`; SQL adapters compose `base_sqlite.rs` or
`base_postgres.rs`. Each payment direction owns its handlers, HTTP routes and
service startup, with common APM and lifecycle components.

With an operator-configured YAML and secrets, start the services independently:

```sh
agentvault --config /etc/agentvault/payer.yaml serve --role payer
agentvault --config /etc/agentvault/payee.yaml serve --role payee
```

Use distinct API/management ports when both processes run on the same host.
`serve` also supports the combined HTTP edge used by the isolated E2E stack.
The payee-only startup path does not load wallet custody keys; reconciliation
workers claim only their configured payment direction.
