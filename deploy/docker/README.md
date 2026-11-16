# AgentVault Docker deployment

The default topology places every AgentVault HTTP API behind AuthGuard's Envoy
edge. AgentVault does not parse login credentials, issue sessions, or evaluate
IAM policies; it verifies AuthGuard's short-lived signed workload context (or
resolves its opaque scope token over AuthGuard's internal gRPC service) and
maps the resulting principal into the payment-domain Client/Scenario hierarchy.

```bash
cp deploy/docker/.env.example deploy/docker/.env
${EDITOR:-vi} deploy/docker/.env
docker compose -p agentvault --env-file deploy/docker/.env \
  -f deploy/docker/docker-compose.yaml up -d
```

The public listener is `http://localhost:8080`: `/auth/*` belongs to
AuthGuard, `/api/v1/*` is forwarded to AgentVault, and `/` serves the Dashboard.
Management port `9091` is loopback-only. Every business SDK uses the same
standard HTTP API through this edge.

PostgreSQL, Redis and Jaeger start by default through
`COMPOSE_PROFILES=postgres,redis,jaeger` in `.env.example`. Remove a profile to
use an external instance, then set `AGENTVAULT__STORAGE__POSTGRES__URL`,
`AGENTVAULT__CACHE__REDIS__URLS` or `AGENTVAULT__MGMT__OTEL__ENDPOINT` in `.env`.
Set `AGENTVAULT__MGMT__OTEL__ENABLED=false` when tracing is not needed. Optional
dependencies do not block the backend when their profiles are disabled.
Jaeger's UI listens on `127.0.0.1:16686`; its default trace store is bounded
memory. Existing PostgreSQL image, volume name and mount path are preserved.

Both edge topologies reuse `components.yaml`. Generic process definitions live
in `middleware/`; the MarketPulse case extends those definitions and supplies
only its own credentials, images, networking and disposable data volume.

For an intentionally identity-free trusted network, use the separate open
topology. It sets `server.authguard.enabled=false`; therefore every HTTP API is
callable without identity. This is explicit and is not the normal deployment.

```bash
docker compose -p agentvault-open --env-file deploy/docker/.env \
  -f deploy/docker/docker-compose.open.yaml up -d
```

The Payee-owned provider and shared `agentvault.chains` entries in the single
canonical [`etc/agentvault.yaml`](../../etc/agentvault.yaml) are operator-owned
examples and must be replaced for a real network. `config/agentvault.yaml` is a
symlink to that file; deployment-specific scalar and secret values belong in
`.env`, not in a copied YAML document. Chain nodes
and the official x402 Facilitator are deliberately not embedded here; the full
disposable stack lives under `use-cases/marketpulse-feed-subscription`.

The CLI can run independent HTTP services with `serve --role payer` and
`serve --role payee`, or both role routers with `serve`. Configure distinct
API and management listeners for two processes on one host. The payee startup
path does not open wallet custody keys. Remove obsolete `agentvault.transport`
settings when upgrading; configuration rejects unknown fields.
