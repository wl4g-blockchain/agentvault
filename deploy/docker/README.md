# AgentVault Docker deployment

The default topology places every AgentVault HTTP API behind AuthGuard's Envoy
edge. AgentVault does not parse login credentials, issue sessions, or evaluate
IAM policies; it verifies AuthGuard's short-lived signed workload context (or
resolves its opaque scope token over AuthGuard's internal gRPC service) and
maps the resulting principal into the payment-domain Client/Scenario hierarchy.
MQTT remains outside HTTP and uses broker ACL plus registered `client_id`.

```bash
cp deploy/docker/.env.example deploy/docker/.env
${EDITOR:-vi} deploy/docker/.env
docker compose -p agentvault --env-file deploy/docker/.env \
  -f deploy/docker/docker-compose.yaml up -d
```

The only public listener is `http://localhost:8080`: `/auth/*` belongs to
AuthGuard, `/api/v1/*` and the optional
`/agentvault.transport.v1.PaymentTransport/*` gRPC service are protected and
forwarded to AgentVault, and `/` serves the AgentVault Dashboard. Management
port `9091` is loopback-only. Set
`AGENTVAULT__AGENTVAULT__TRANSPORT__GRPC__ENABLED=true` to enable that gRPC
listener; clients still connect through Envoy rather than port `50051`.

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
disposable stack lives under `use-cases/marketpulse-feed-subscription/e2e`.

MQTT is disabled in this general-purpose stack. Enable it only after pointing
AgentVault at a broker with password/TLS authentication and per-client topic
ACLs. The E2E stack provisions and verifies that independent `client_id`
boundary against EMQX; HTTP AuthGuard identity is never reused as MQTT trust.
