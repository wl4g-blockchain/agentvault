# MarketPulse Feed Subscription verifier

## Functionality and execution

The case verifies QuantStream payers buying MarketPulse news through AgentVault. It includes five SDK languages (Rust, Go, Python, TypeScript, Java), both HTTP control planes, x402/EVM on Anvil, x402/Solana with SPL tokens, MPP/Tempo charge and recovery, A2A boundaries, and Chromium dashboard journeys. A full isolated matrix contains 25 cases: 5 payers × 5 payees × 2 transports. The default simple scope contains 4 cases.

Deployment assets are grouped horizontally by service. Mock production sources and dependency manifests live in `deploy/<service>/source/`; native contracts are owned by their vertical scenarios under `verifier/<scenario>/contracts/`; SDK dependencies remain in the repository's `src/sdk/`. AgentVault's Dockerfile and Dockerfile.dockerignore link to production assets. Its component directory contains only those links, config/ overlays and docker-compose.yaml; Helm uses the repository's deploy/helm/agentvault directly. Middleware packages use upstream images and need no copied upstream source. Shared Helm overlays are configuration bundles, not additional workloads.

Verification is grouped vertically by scenario. Each `E2ES<NN><Description>Verifier` exposes `deploy(context)`, `init(context)`, and `verify(context)`. The runner calls them in that order and closes temporary forwarding sessions even on failure. Unused phases remain explicit no-op methods. Payment assertions stay in scenarios; `util/` contains reusable class operations. `verifier/context.py` owns case identities, resource fixtures, shared context and the runtime contract.

All scenario functions, including parsers and fixture helpers, are owned by their verifier class. Internal helper classes are nested under that owner; `util/` exposes only the prescribed utility classes. Infrastructure startup runs in `deploy`; chain funding, ACL setup and catalog imports run in `init`; payment requests and evidence assertions run in `verify`. The Web preview is deployed before its report-derived API fixtures are initialized. Custom EVM scripts, Solidity source and compiled artifacts live together under `deploy/evm/source/`.

`util/images.py::E2EImagesDefinitions` is the sole image catalog: `IN_CN_GFW` (also accepts `IN_GFW`) / `NON_GFW` with explicit Aliyun/official references. All macOS/Linux/Windows components use linux/amd64, except macOS Solana uses linux/arm64. Auto selection probes Docker Hub directly. Runtime images default to AMD64, including macOS and Windows; only Solana on macOS uses `registry.cn-shenzhen.aliyuncs.com/wl4g/anza_solana:3.1.14-arm64`. Compose receives environment/build arguments and Helm receives explicit image values, including Envoy sidecars. Docker discovers the active native, OrbStack or Colima context; Kubernetes detects native Docker/containerd/K3s, OrbStack and Colima stores and checks architecture before importing local images. Remote nodes are rejected when a local image store cannot be verified.


The purchase journey owns signing, settlement, balance observation and accounting. Its Docker and Kubernetes environment implementations share the same business assertions; Kubernetes does not inherit a Docker runtime. `x402-evm` is an alias of `full`, so selecting both runs the purchase once. `payer`/`payee` remain compatibility commands for dependency smoke checks. Infrastructure components never become `verifier/storage`, `verifier/cache` or backend-specific scenarios.

Additional vertical scenarios cover authorization projection/Origin/expiry rejection and stable replay, approval decision boundaries, Price revisions and history, RSA Wallet import, signed identity isolation, delivery recovery and settled-payment restart. `budget-race`, `key-lifecycle` and `outbox-redelivery` exercise the real domain/SQL transaction contracts on SQLite and an isolated temporary PostgreSQL database. They complement the chain purchase and do not claim an additional live-chain or HTTP purchase for SQLite. The identity scenario uses a controlled issuer for the real Vault AuthGuard-context verifier; it does not claim to exercise AuthGuard IAM, the opaque-scope resolver or a deployed edge Gateway.

`metrics`, `tracing`, `logs`, the purchase stages and the new boundary scenarios automatically require `full` in the same run. Purchase captures raw OpenMetrics before/after payment, verifies operation counters against histogram counts and expected signatures/approval, queries every logged trace through Jaeger with 100% sampling, and checks the SDK-producer → Vault-consumer parent link. Logs bind the same runs across SDK, Vault and merchant. These observations are captured in the purchase window before fault scenarios restart the process. `live-wallet` uses real Chromium Web Crypto and forwards browser API requests to the live Vault, then checks PostgreSQL custody. The existing `web` scenario retains report-derived fixtures for its matrix presentation checks.

```text
marketpulse-feed-subscription/
├── runner.py
├── requirements.txt
├── docker-compose.yaml
├── deploy/
│   ├── agentvault/{Dockerfile->production,Dockerfile.dockerignore->production,config/,docker-compose.yaml}
│   ├── <middleware>/{Dockerfile,docker-compose.yaml,helm/}
│   ├── evm/{source/{src/,artifacts/,*.sh},Dockerfile,docker-compose.yaml,helm/}
│   ├── <mock-service>/{source/,Dockerfile,docker-compose.yaml,helm/}
│   ├── mock-payee-marketpulse-shared-helm/{e2e-envoy.configmap.yaml,e2e-values.yaml}
│   └── mock-payer-quantstream-shared-helm/{e2e-envoy.configmap.yaml,e2e-values.yaml}
├── util/{process,images,postgres,pgvector,sqlite,kubernetes,docker,report,constant}.py
├── verifier/
│   ├── context.py
│   ├── infra/s{00..02}_*.py
│   ├── purchase/s{20..24,30}_*.py
│   ├── authorization/s{11,16}_*.py
│   ├── approval/s12_approval_boundary.py
│   ├── pricing/s13_price_governance.py
│   ├── custody/s{14,17}_*.py
│   ├── identity/s15_identity_boundary.py
│   ├── delivery/s32_delivery_recovery.py
│   ├── recovery/s{33,34}_*.py
│   ├── tempo_charge/s25_charge_recovery.py
│   ├── solana_settlement/s26_exact_settlement.py
│   ├── a2a_payment/s27_message_payment.py
│   ├── acceptance/s29_retained_purchase.py
│   ├── dashboard/s{40,41}_*.py
│   └── otel/s{50..52}_*.py
└── reports/{<UTC-run-id>/,archive/}
```

Commands run from the repository root.

```bash
CASE=use-cases/marketpulse-feed-subscription
python3 -m venv /tmp/agentvault-e2e-venv
source /tmp/agentvault-e2e-venv/bin/activate
pip install -r "$CASE/requirements.txt"
python -m playwright install chromium
python "$CASE/runner.py" --list
E2E_IMAGE_NETWORK=IN_CN_GFW python "$CASE/runner.py" run \
  --deployer kubernetes --scenario structure,infrastructure
E2E_IMAGE_NETWORK=IN_CN_GFW python "$CASE/runner.py" run \
  --deployer kubernetes --scenario matrix,mpp-tempo,x402-solana,a2a,x402-evm,web \
  --e2e-sdk-scope full --matrix-workers 1 --timeout 45m --reuse-build \
  --cleanup-after-run
```

Use `--deployer docker` for Compose. `--skip-build` requires all selected platform images to exist; `--reuse-build` reuses them when present. Resume an interrupted matrix with `--scenario matrix --resume-run <UTC-run-id>` and the same scope/backend. For Web verification of an earlier matrix, set `--matrix-evidence-run <UTC-run-id>`.

For manual review, run `--scenario manual --deployer kubernetes` to retain services without triggering payer payments. `manual-services` resumes existing services; `manual-payment` makes one explicit purchase; `manual-matrix` runs 25 cross-language purchases on the retained stack. Use `--payment-profile evm|solana|tempo`, `--message-protocol mcp|a2a`, and the language/transport flags to select a purchase. Finish with `runner.py cleanup --deployer kubernetes`.

For a standalone component build, run `python "$CASE/util/images.py" -- docker buildx build --load -f "$CASE/deploy/<service>/Dockerfile" .`. The helper injects all declared image build arguments and the catalog output tag. Solana is built from a clean official Agave checkout as its build context, with matching version/revision; see its Dockerfile. New images in restricted networks should be fetched through `~/mirror-pull.sh`; build-time proxy: `HTTPS_PROXY=http://127.0.0.1:8800`. Check Rust target directories before compiling; clean those exceeding 20 GiB.

For an existing Chromium installation, set `E2E_CHROMIUM_EXECUTABLE` to its executable path; the Web report records the actual browser version.

The twelve mock test trees have been moved out of deployment sources without deleting assertions: payer/payee contracts live in `verifier/purchase/contracts/`, approval UI contracts in `verifier/approval/contracts/`, provider contracts in `verifier/tempo_charge/contracts/`, and Solana projections in `verifier/solana_settlement/contracts/`. Rust `#[cfg(test)]` paths, Java testSourceDirectory, Node test scripts and native Go test modules point there. Production TypeScript builds include only src; contracts use the production component's dependencies. No scenario was added or removed for this move.

Native contract examples (from the repository root):

```bash
CASE=use-cases/marketpulse-feed-subscription
go -C "$CASE/verifier/purchase/contracts/marketpulse_golang" test ./...
cargo test --locked --manifest-path "$CASE/deploy/mock-payee-marketpulse-rust-service/source/Cargo.toml"
mvn -f "$CASE/deploy/mock-payee-marketpulse-java-service/source/pom.xml" test
npm --prefix "$CASE/deploy/mock-payee-marketpulse-typescript-service/source" test
PYTHONPATH="$CASE/deploy/mock-payee-marketpulse-python-service/source" python -m unittest discover -s "$CASE/verifier/purchase/contracts/marketpulse_python" -p '*_test.py'
node --test "$CASE/verifier/approval/contracts/quantstream_ui/approval_support.test.mjs"
```

Prepare the local SDK and component dependencies before invoking native suites. The GFW catalog never silently falls back overseas. NON_GFW uses upstream registries; only macOS Solana and locally built fixtures are exceptions. Explicit custom image outputs are retained. Switching network mode reprojects previously generated defaults. The local `solana:3.1.14-arm64` alias is treated as local and never pulled from Docker Hub.

## Independently reviewing report evidence

Each run writes `reports/<UTC-run-id>/summary.md` and scenario Markdown. A report is created only after its verifier returns successfully. Failed runs can retain diagnostic files without a successful summary. Completed matrix cases have atomic `matrix-cases/case-NN.json` checkpoints bound to `matrix-plan.json`; resumption rejects a changed scope or backend. `E2EReportHelper.archive` moves older completed runs to `reports/archive/`, retaining the latest 20; current/evidence runs remain protected. Historical evidence preserves its original paths and timestamps.

New purchases seal their public SQL observations, financial JSON, redacted logs and raw OTEL responses in `payment/full-flow/<payer>-<payee>-<transport>/manifest.json`, with content hashes, source revision/working-tree digest and actual image identities. Matrix checkpoints reference and revalidate this manifest; changed source or evidence prevents skipping payment. Historical checkpoints without manifests remain readable as historical reports but cannot be resumed as newly verified evidence. Hashes identify content changes; independent SQL/chain review remains the authority.

Review these facts together:

1. Confirm the selected backend, scope, source revision, image platform/identity and run ID. Pod readiness proves availability only.
2. Recompute matrix coverage from `matrix-plan.json` and checkpoints: 50 unique payer/payee/transport tuples for full, 8 for simple. Each case must preserve signing, settlement, balance-sync and gateway-balance assertions; every expected tuple must appear exactly once.
3. Follow the payment `run_id`, intent ID, scenario, wallet and transaction hash through SDK logs, Vault PostgreSQL intent/ledger records, business SQLite delivery records and the independent chain RPC receipt. Check identity and amounts, not just HTTP status or log keywords.
4. Verify payer debit, merchant credit, facilitator/native gas and Token Gas accounting against chain balances before/after. Recovery must reuse settlement while delivering exactly once. SQLite evidence must use a consistent backup, including committed WAL changes, and be queried read-only.
5. For Anvil, independently fetch `eth_getTransactionReceipt` and `eth_call balanceOf`; confirm chain 31337 and the expected ERC-20/method. For Solana, fetch `getGenesisHash`, confirmed `getTransaction`, `getTokenAccountBalance` and `getBlockTime`: bind mint, ATA, token amount and fee payer. For Tempo, verify chain 1337, actual charge receipt, pathUSD balance and Token Gas separately.
6. Web screenshots demonstrate browser behavior. Matrix views consume recorded evidence; controlled UI API routes do not independently prove live settlement. A2A SDK boundary tests also need the payment scenarios for real-chain claims.

The retained stack's 15 merchants and five payer services use standard HTTP. Do not label its 150 profile/message/transport/language combinations as every possible two-sided control-plane combination. Challenge inspection, Ready Pods and SDK unit tests cannot establish completed purchase coverage.

Reproduction examples (with retained services):

```bash
python "$CASE/runner.py" run --deployer kubernetes --scenario manual-payment \
  --payer-language python --payee-language java --payment-profile evm --transport http
kubectl -n e2e-agentvault-system get pods
kubectl -n e2e-agentvault-payment-system get jobs,pvc
kubectl -n e2e-agentvault-system port-forward service/agentvault-management 19091:9091
```

Use management read APIs for intent/ledger inspection; the management port does not accept business payment writes. Query PostgreSQL independently using `kubectl exec` and `psql`, and snapshot business SQLite using its backup API. Export public receipts and redacted logs; never export custody Secrets or raw credentials. Before retained-stack purchases, verify the local chain clock is within 60 seconds of the host. A paused Solana ledger may need time to catch up; do not bypass freshness checks.

[中文说明](README_CN.md#报告真实性完整性与可靠性复验) contains the complete executable protocol challenges, SQL, receipt, approval/rejection and retained-ledger review commands in this same README pair.
