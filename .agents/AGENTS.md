# General Coding Guidelines

[Chinese translation](AGENTS_CN.md)

- 1. You MUST strictly follow the file and directory structures specified in every section below.
- 2. Prefer libraries: use official or established community libraries for domain capabilities. Reuse their data structures and protocol or core workflow logic directly. Implement custom logic only when a suitable library is unavailable. Extend existing types through inheritance or composition instead of duplicating attributes with the same meaning.
- 3. Organize capabilities by domain with one-way dependencies, high cohesion, low coupling, and consolidated responsibilities. Each enabled capability needs configuration, contracts, tests, resilience, and observability.
- 4. **IMPORTANT:** The code volume of any official source code file (`rs|go|py|java|ts`) under src **MUST NOT exceed `800` lines** (excluding comments); otherwise, it indicates that the code **you generated is of unqualified quality** and must be redone to be refined and converged while also reasonably split. It is forbidden to split arbitrarily without satisfying the following directory structure specification. Each source code file must be named reasonably and standardly (if there are internal source code files of a module that is reasonably split, they must also reflect the hierarchical relationship; otherwise, it will be considered an **non-compliant split and MUST BE DELETED**). **It is forbidden to satisfy the specification at the cost of weakening functionality**.

## 0. Global Structure for Multiple Submodules

```text
<src-dir-as-(rust|java|typescript)-source-root>|<pkg-dir-as-golang-source-root>|<app-dir-as-python-source-root>/
    ├── [sdk/{rust/,golang/,python/,java/}]  # Client SDKs for multiple languages (If Need else delete this sdk directory).
    ├── common/<src|pkg|app>/{[audit-log/,error/,macro/,mem-prof,pprof/,telemetry/],...}
    ├── utils/<src|pkg|app>/
    ├── types/<src|pkg|app>/
    ├── config/<src|pkg|app>/
    └── core/<src|pkg|app>/
        ├── context/
        ├── mgmt/
        ├── cache/
        ├── storage/{postgres.(rs|go|py|ts),sqlite.(rs|go|py|ts),[mongo.(rs|go|py|ts),timescaledb.(rs|go|py|ts)]}
        ├── modules/<domain>/{route/,handler/,storage/}  # At most 500 lines of code per source file, excluding comments.
        └── ...
    ├── <module>/<src|pkg|app>/
    ├── cmd/<src|pkg|app>/{[bin/<myapp>.(rs|go|py|ts)],cmd/{console.(rs|go|py|ts),...}}
    └── web/<src|pkg|app>/
        ├── deploy/{Dockerfile,nginx.conf}
        ├── public/
        ├── <src|pkg|app>/
        ├── tests/
        └── ...
use-cases/<case-name>/e2e/
    ├── common/{context.py,<domain>/*.py,}  # Shared helpers for this E2E case; Python files only.
    ├── config/{<myapp>/{init_0x.(ddl|dml).sql,init.import.yaml,values-override.yaml},<domain>/<file>.(json|yaml|conf|toml|env|gz)}  # Domain-specific initialization and runtime configuration.
    ├── deploy/
    │   ├── deployer/{base.py,docker.py,kubernetes.py}  # Both deployment modes, with corresponding XxxCleaner cleanup implementations.
    │   ├── <middleware>/{docker-compose.yaml,helm/}
    │   └── mock-<external>-service[-<language>]/{(src|pkg|app)/{<package>/,Dockerfile,docker-compose.yaml,helm/}}
    ├── verifier/{infra/s00_infra.py,infra/s01_structure.py,<module>/sNN_<scenario>.py,web/sNN_<scenario>.py}  # Web verifiers MUST use Playwright and capture real UI screenshots.
    ├── reports/<yyyyMMddHHmmss>/{summary.md,<module>/s0x_<scenario>.md}
    ├── README.md
    ├── README_CN.md
    ├── requirements.txt  # One unified dependency definition.
    └── runner.py  # The only E2E execution entry point.
Makefile  # Required for non-Java projects.
benches/  # Required for Rust, Go, and Python projects.
...
```

- 0.1. Dependencies follow the layers from top to bottom by default.
- 0.2. In this notation, `<>` is required, `[]` is optional, `|` selects one alternative, `,` lists multiple entries, and `{}` specifies a directory level, similarly to regular expressions or GNU command syntax.
- 0.3. `runner.py` owns the CLI, deployment, reporting, and cleanup. Support multiple `--scenario` values, rounds, deployer selection, timeouts, build reuse, and cleanup as needed. Generate timestamped reports. Each verifier exposes only `verify(context)` and follows the domain-specific `sNN_` convention. Preserve the environment for acceptance by default.

## 1. Single YAML Configuration

- Use typed, layered configuration, `snake_case` keys, `<MYAPP>__` environment variables, and Secrets for sensitive values.

```text
config/
└── config.(go|rs|py|ts) # The single entry point for types, defaults, and environment overrides.

<App>Config { server,mgmt,logging,cache,storage,secrets,<myapp>/<domain>... }
```

- Keep types and YAML at the same layer, organized from infrastructure to domain to business configuration. Precedence is defaults < YAML < environment variables. Reject unknown fields.

```yaml
server: { host: 0.0.0.0, port: 8080 }
mgmt: { enabled: true, host: 127.0.0.1, port: 9091,
  health: { liveness_path: /healthz, readiness_path: /readyz }, metrics: { enabled: true, path: /metrics },
  otel: { enabled: false, endpoint: "", protocol: grpc, timeout: 5s, sample_rate: 1.0 } }
logging: { mode: HUMAN, level: info } # HUMAN|JSON
storage:
  provider: Postgres # Postgres|SQLite
  postgres: { url: "...", username: "${MYAPP__STORAGE__POSTGRES__USERNAME:postgres}", password: "${MYAPP__STORAGE__POSTGRES__PASSWORD:}", pool: { max: 20, timeout: 5s } }
  sqlite: { url: "...", pool: { max: 1, timeout: 5s } }
cache:
  provider: Memory # Redis|Memory
  redis: { url: "...", username: "...", password: "...", pool: { max: 20, timeout: 3s } }
  memory: { max_capacity: 65535, default_ttl: 1h, cleanup_interval: 30s }
<myapp>: ...
```

- Support `${MYAPP__A__B:default}` interpolation in scalar values. Expose management health and metrics only on private networks, with no write operations. Logs include time, level, service, component, run, trace, and span fields, with sensitive data redacted.

## 2. Domain Components

- Every independent domain component or submodule MUST use the shared Provider SPI architecture. Follow the `<domain>/src/` structure below strictly.

```text
src/
├── common/
│   ├── context/
│   │   ├── src/
│   │   │   ├── lib.rs                   # Public module and type exports.
│   │   │   └── lifecycle/
│   │   │       ├── application.rs       # ApplicationContext registration, retrieval, and lifecycle management.
│   │   │       └── provider.rs          # The top-level IProvider contract.
│   ├── audit-log/src/
│   ├── mem-prof/src/
│   ├── ...
├── <domain>/src/
│   ├── lib.rs                           # Independent domain module and type exports.
│   ├── <domain>.rs                      # Domain interface I<Domain>: IProvider.
│   └── <domain>/
│       ├── <spec_impl_name_1>.rs        # <SpecImplName1><Domain>.
│       └── <spec_impl_name_2>.rs        # <SpecImplName2><Domain>.
│       └── <spec_impl_name_N>/
│           └── <meaningful_name>.rs     # Optional private modules for complex internal logic only.
└── ingestor/src/                        # Example ingestion domain module.
    ├── lib.rs
    ├── ingestor.rs                      # ITestIngestor: IProvider.
    └── ingestor/
        ├── web.rs                       # WebTestIngestor: ITestIngestor.
        └── twitter.rs                   # TwitterTestIngestor: ITestIngestor.
        └── twitter/
            └── twitter_client.rs        # Internal Twitter integration through a third-party library.
```

- Usage example:

```rust
let ctx = ApplicationContext::global(); // All application modules share this process-level context.
ctx.register::<dyn ITestIngestor, _>("web-default", web_ingestor_instantiator)?;
ctx.register::<dyn ITestIngestor, _>("twitter-default", twitter_ingestor_instantiator)?;
ctx.init().await?; // The entry point starts the context; providers remain lazy until their first get.
let web = ctx.get::<dyn ITestIngestor>("web-default").await?;
let twitter = ctx.get::<dyn ITestIngestor>("twitter-default").await?;
let page = web.ingest("https://example.test/news/42", "Market update");
let tweet = twitter.ingest("tweet-42", "Latest update");

// Stop accepting work and wait for active tasks before closing providers.
ctx.close_for::<dyn ITestIngestor>("web-default").await?; // Optional: close one instance.
ctx.shutdown().await?; // Close the remaining instances and stop the context.
```

- The ingestion example uses `ITestIngestor`, `WebTestIngestor`, and `TwitterTestIngestor`. Domain interfaces inherit `IProvider`; registered asynchronous constructors return `Result<Arc<dyn ITestIngestor>>`.
- Each domain module obtains the same context through `ApplicationContext::global()`. The application entry point owns registration, `init`, and `shutdown`. Accessing `global()` neither starts the context nor replaces it after shutdown. Use `ApplicationContext::new()` for deliberately independent contexts, such as isolated tests.
- The registry is indexed by domain interface type and then `instance_id`. Do not pass a separate implementation name; use different instance IDs for different implementations in the same domain.
- Expose multiple interfaces of one object through `register_with::<Target, Source>(instance_id, projection)` to share its lifecycle.
- **IMPORTANT: In production Go, Rust, and Python source files, define an appropriate struct or class first and place functions in its implementation; do not define standalone functions directly.**

## 3. Storage and Repositories

```text
storage/
├── storage.(go|rs|py|ts)     # IGenericRepository contracts, generic CRUD operations, factory behavior, and shared SQL composition.
├── postgres.(go|rs|py|ts)    # Generic PostgreSQL repository implementation, reusable or overridable by domain repositories.
├── sqlite.(go|rs|py|ts)      # Generic SQLite repository implementation.
└── <domain>/
    ├── postgres.(go|rs|py|ts) # Domain PostgreSQL repository; reuse base CRUD operations first; at most 250 lines.
    └── sqlite.(go|rs|py|ts)   # Domain SQLite repository.
```

- `IGenericRepository` MUST provide generic CRUD contracts such as `select(entity,page)`, `select_by(id)`, `insert(entity)`, `update(entity)`, `upsert(entity)`, and `delete_by(id)`, using reflection or generated macros where applicable.

## 4. Cache

```text
cache/
├── cache.(go|rs|py|ts)  # ICache contracts, factory behavior, and shared operations such as get/set/delete/exists/clear/close.
├── redis.(go|rs|py|ts)  # RedisCache adapter for Redis single-node and cluster modes.
└── memory.(go|rs|py|ts) # MemoryCache adapter.
```

## 5. Jobs and Scheduling

- `job/<domain>.(go|rs|py|ts)` handles only cron scheduling, concurrency, locking, lifecycle, observability, and component invocation. Skip execution when a lock cannot be acquired. Reject invalid cron expressions at startup. Components own processing, idempotency, and retries.

## 6. Observability and OpenLineage

- `run_id` is the shared, publicly exposable idempotency and correlation identifier. It may appear in logs, business tables, spans, and OpenLineage facets. OpenTelemetry MUST NOT enforce business constraints or recovery.
- Do not create a separate correlation identifier. HTTP/gRPC carry their own matching context. For asynchronous messages, `requestSpanId` MUST equal the producer `span_id` in W3C `traceparent`; responses echo it unchanged and create a consumer span. Do not persist it or use it for business constraints, except as a short-lived response-cache key for MQ QoS retries. Retries create new spans while keeping `run_id` and incrementing `attempt_no`.
- Propagate W3C Trace Context across service calls. Record low-cardinality counters, histograms, and gauges for important transitions, including outcomes, latency, and errors.

| Field or scenario | OLTP transactions | OLAP / ETL |
|---|---|---|
| Steps | Payment → approval when needed → execution → callback → reconciliation | UI → API → Airflow Driver DAG → N Worker DAGs → one Flink job per worker → ODS |
| `run_id` | Fixed for one payment, including retries, restarts, approval, and reconciliation | Independent per worker; retained by Flink and written to ODS for DWD lineage |
| `trace_id` | Long-running transactions trace separate stages: **1 runId → N traceIds** | Continuous propagation across workers: **1 traceId → N runIds** |
| `span_id` | Managed by OpenTelemetry; separate for each call, message processing step, or retry | Same |
| `parent_span_id` | References only a parent span, never a business identifier | Same |
| Shared rules | Run and trace identifiers have no fixed hierarchy; retries retain `run_id`, increment `attempt_no`, and create a new span | Same |

## 7. Client SDKs for Multiple Languages (If Need else delete this sdk directory)

```text
src/sdk/
├── config/config.(go|rs|py|ts)
├── model/{base,<domain-a>,<domain-b>}
├── <domain-a>/{<domain-a>,[ecosystem]}
├── <domain-b>/{<domain-b>,[standard],[ecosystem]}
├── transport/{http,grpc,mqtt}
├── utils/
└── <facade>.(go|rs|py|ts|java)
```

- Keep one state machine in the main file. Adapters convert types. Transport owns communication, timeouts, tracing, and errors. The facade is the unified entry point.

## 8. Makefile

- Image, E2E, and release tasks support an overridable `IN_CN_GFW` detection: registry HTTP 200/401 means false; other results mean true. `IMAGE_SOURCE` selects aliyun or global registries and propagates proxy settings.

## 9. Unit Tests

- Provide corresponding unit tests for source files. Mirror the source layout under `tests/`; avoid inline, oversized, or orphaned tests.

```text
├── src/<package>/<file>.(go|rs|py|ts)
└── tests/<package>/<file>_test.(go|rs|py|ts)
```

## 10. Modular Helm Charts

```text
helm/<chart>/
├── {Chart.yaml,values.yaml}
├── bootstrap/{common,setup-secret-<provider>.sh,*.env.example}
└── templates/{_helpers.tpl,platform/,gateway/,web/,<module>/}
```

- Split templates by domain and layer. Keep ConfigMaps and Secrets separate. Deployment annotations include configuration checksums.
- Before installation, `bootstrap/` creates or synchronizes K8s, GCP, AWS, or Vault secrets with least privilege. Installation MUST fail if bootstrap fails. Never place plaintext secrets in charts, values, or logs.

## 11. CI Pipelines

| CI | Trigger and checkout | Behavior | Artifact boundary |
|---|---|---|---|
| PR | Created, updated, or reopened PR, using its head SHA; cancel superseded runs | Build `dirty-<sha8>`; unit tests and E2E reuse the image | Only temporary dirty-build evidence; no `latest` tag |
| Merged | Merge SHA; serialized execution | Do not repeat tests; build binaries, multi-architecture images, and Helm charts | Publish version tags, `latest`, and OCI charts only after all builds succeed; then pull them back for verification |

- Upsert one persistent comment per PR, covering all results and evidence. Identify it with a marker and run the reporter with `if: always()`, reading `needs.*.result`.

## 12. Quality Gates

```text
Structure and file matrix → APIs and dependency direction → formatting and lint → unit tests → integration tests → configuration and deployment rendering → E2E → cleanup
```

- Reject redundant source files and obsolete symbols. Each write has one owning component. Verify out-of-order handling and idempotency. Keep code, configuration, migrations, deployment, and documentation synchronized. Require all checks to pass and complete cleanup.
