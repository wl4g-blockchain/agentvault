# AgentVault Helm Chart

Templates follow workload ownership rather than Kubernetes-kind flat files:

```text
templates/
├── platform/  # optional runtime ConfigMap, Secret projection, ServiceAccount
├── server/    # AgentVault Deployment, API/management Services, NetworkPolicy, ServiceMonitor
├── gateway/   # protected API/gRPC HTTPRoute
├── web/       # Dashboard Deployment, Service and protected Route
├── _helpers.tpl
└── NOTES.txt
```

This is a packaging boundary only: the split does not change rendered object
names, selectors, ports, routes, security context, checksums or conditions.
`bootstrap/` remains outside templates because runtime Secrets must exist before
the chart is installed; Helm never generates payment keys or stores their values.

AuthGuard is a default-on umbrella dependency. The Gateway authenticates and
authorizes HTTP requests, then delivers a signed `x-authguard-context` or a
short-lived `x-authguard-scope-token` to the AgentVault workload adapter. The
`AGENTVAULT__SERVER__AUTHGUARD__DIRECT_CONTEXT_HMAC_KEY` value in the
AgentVault secret must be identical to
`AUTHGUARD__AUTHZ__SCOPE_DELIVERY__DIRECT_CONTEXT_HMAC_KEY` in the separately
managed AuthGuard secret.

Direct contexts are verified locally. Token-backed contexts are resolved over
AuthGuard's internal scope gRPC service; the workload pod carries the
`authguard.io/scope-client=true` label required by the dependency NetworkPolicy.
If the dependency fullname is overridden, set
`agentvault.backend.env.AGENTVAULT__SERVER__AUTHGUARD__SCOPE_RESOLVER__TARGET`
to its `scope-grpc` service address.

AgentVault itself owns only payment-domain checks: Client/Scenario ownership,
Policy, approval, balance, signing, settlement, and Ledger invariants.

`agentvault.payer` and `agentvault.payee` in the native runtime configuration
are independent modes. A
Facilitator or MPP Provider is configured only under its Payee adapter, while
`agentvault.chains` is the shared operator-owned chain/RPC registry used by both
directions. Wallet key management contains KEK references only; encrypted EOA
key versions remain in the authoritative Wallet SQL aggregate.

## Values and environment names

The values file has three top-level ownership boundaries only:

- `global`: settings shared by chart workloads.
- `agentvault.backend`, `agentvault.frontend`, and
  `agentvault.agentvault-config`: backend Kubernetes resources, frontend
  Kubernetes resources, and an optional native `agentvault.yaml` override.
- `authguard`: the unmodified AuthGuard dependency values contract; the
  dependency supplies the authentication/authorization middleware.

By default the chart does not duplicate or mount runtime defaults: the process
uses the canonical `etc/agentvault.yaml` embedded in the runtime image. Set
`agentvault.agentvault-config` only when a deployment needs a complex native
YAML override; a non-empty YAML block creates and mounts a ConfigMap.
Runtime precedence is built-in defaults < selected native YAML < process
environment. Every scalar AgentVault override uses the canonical field path
with double underscores; for example
`AGENTVAULT__SERVER__REQUEST__TIMEOUT=15s`. Single underscores remain part of a
snake_case field and unknown fields are rejected.

Sensitive leaves use that same canonical name. For example,
`storage.postgres.url.env` names `AGENTVAULT__STORAGE__POSTGRES__URL`, and the
matching Kubernetes Secret key contains the actual PostgreSQL URL. The loader excludes
that value from the serializable `AppConfig` and resolves it only through the
configured `SecretRef`. There is no second helper/slot naming convention and
no legacy alias fallback. Wallet encryption keys, recovery keys, approval
signing keys, provider credentials, and MQTT passwords follow the same rule.

`agentvault.backend.env` is reserved for deployment-specific scalar overrides.
Complex maps and lists may be supplied as native YAML through
`agentvault.agentvault-config`.
Secret values must never be placed in Helm values because Helm stores release
values.

## Import declarative payment resources

`agentvault --config PATH console import FILE_OR_DIRECTORY...` imports canonical
`{consoleVersion, kind, metadata, data}` Client, Wallet, Scenario, Policy,
Price, and Approver resources into the configured SQL authority. Each YAML
document or JSON file describes exactly one resource. Local-wallet documents
contain only a narrowly scoped `private_key_env` reference; key bytes are read
at command execution and persisted only through the versioned wallet-key
envelope encryption boundary.

With `agentvault.backend.resourceImport.enabled=true`, the chart runs the same
command in a `resource-import` init container before `serve`. Exact replays are
idempotent and drift fails closed. Import never submits a payment, mints or
funds an asset, or resets Ledger state. Chain fixtures remain separate jobs.

The chart defaults to `agentvault.backend.resourceImport.enabled=false`.
Operators may point it at an operator-owned ConfigMap or run the console command
separately. This is a management/import facility, not service provisioning.

## Bootstrap runtime secrets

Create one single-line `KEY=VALUE` payload from the checked-in template. The
wallet KEK, recovery-envelope key, and approval JWT key are independent
unpadded base64url encodings of 32 random bytes.

```bash
cp deploy/helm/agentvault/bootstrap/agentvault-secrets.env.example agentvault-secrets.env
${EDITOR:-vi} agentvault-secrets.env
```

The AuthGuard context HMAC value must equal the corresponding value in the
separately managed AuthGuard Secret. AgentVault does not provision or read
AuthGuard's JWT, provider, PostgreSQL, or Redis credentials.

Kubernetes Secret is the default provider:

```bash
deploy/helm/agentvault/bootstrap/setup-secret-k8s.sh \
  --secret-file "$PWD/agentvault-secrets.env"
```

Managed providers store exactly the same payload and produce a non-secret Helm
| Source | Setup command | Generated values |
| --- | --- | --- |
| GCP | `setup-secret-gcp.sh --project PROJECT --account ACCOUNT --secret-file FILE` | `agentvault-gcp-values.yaml` |
| AWS | `setup-secret-aws.sh --cluster EKS_CLUSTER --secret-file FILE` | `agentvault-aws-values.yaml` |
| Vault | `setup-secret-vault.sh --secret-file FILE` | `agentvault-vault-values.yaml` |

GCP and AWS require the Secrets Store CSI driver and matching provider. Vault
requires the Vault Agent Injector and preconfigured Kubernetes auth mount. The
setup scripts create only application-scoped secrets, reader identities,
policies and bindings; cluster-wide trust remains platform-owned. Use
`--skip-kubernetes-mirror` when the managed provider must remain the only copy.

Install the default Kubernetes-backed chart with:

```bash
helm upgrade --install agentvault deploy/helm/agentvault \
  --namespace agentvault --create-namespace \
  --set agentvault.backend.secrets.kubernetes.existingSecret=agentvault-secrets
```

Append `-f agentvault-<provider>-values.yaml` for GCP, AWS, or Vault. Never put
secret values in Helm `--set` arguments or values files.

To intentionally deploy without HTTP identity enforcement:

```bash
helm upgrade --install agentvault deploy/helm/agentvault \
  --set authguard.enabled=false \
  --set agentvault.backend.route.enabled=false \
  --set agentvault.frontend.route.enabled=false \
  --set agentvault.backend.service.type=LoadBalancer
```

That mode is fully open on the API Service by design; the chart's NetworkPolicy
opens only API and optional gRPC ingress, while management remains limited to
the configured Envoy/monitoring selectors. MQTT remains a separate
broker/client-id boundary in both modes.

`/healthz` is process liveness only. `/readyz` performs a bounded probe against
the authoritative SQL adapter (`mgmt.health.probe_timeout`, default `1s`)
and returns 503 on failure or timeout. Redis is deliberately not a readiness
gate because it is a non-authoritative acceleration layer.

The SDK gRPC listener is disabled by default. Setting
`agentvault.backend.env.AGENTVAULT__AGENTVAULT__TRANSPORT__GRPC__ENABLED=true`
adds an h2c ClusterIP port and a protected Gateway route for
`/agentvault.transport.v1.PaymentTransport/*`. AuthGuard authenticates that
route and injects the same signed `x-authguard-context` metadata; the NetworkPolicy
accepts the internal h2c port only from the configured Envoy selector. The
Envelope accepts only domain-authorized SDK payment routes and never exposes
`/admin/*`. Do not expose port `50051` directly in the normal protected mode.
