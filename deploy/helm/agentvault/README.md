# AgentVault Helm Chart

AuthGuard is a default-on umbrella dependency. The Gateway authenticates and
authorizes HTTP requests, then delivers a signed `x-authguard-context` to the
AgentVault workload adapter. The
`AUTHGUARD__AUTHZ__SCOPE_DELIVERY__DIRECT_CONTEXT_HMAC_KEY` value in the
AgentVault secret must be identical to the value in the separately managed
AuthGuard secret.

AgentVault itself owns only payment-domain checks: Client/Scenario ownership,
Policy, approval, balance, signing, settlement, and Ledger invariants.

`runtime.payer` and `runtime.payee` are independent deployment modes. A
Facilitator or MPP Provider is configured only under its Payee adapter, while
`runtime.chains` is the shared operator-owned chain/RPC registry used by both
directions. Wallet key management contains KEK references only; encrypted EOA
key versions remain in the authoritative Wallet SQL aggregate.

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
override file:

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
  --set secrets.kubernetes.existingSecret=agentvault-secrets
```

Append `-f agentvault-<provider>-values.yaml` for GCP, AWS, or Vault. Never put
secret values in Helm `--set` arguments or values files.

To intentionally deploy without HTTP identity enforcement:

```bash
helm upgrade --install agentvault deploy/helm/agentvault \
  --set authguard-middleware.enabled=false \
  --set routes.enabled=false \
  --set service.type=LoadBalancer
```

That mode is fully open by design. MQTT remains a separate broker/client-id
boundary in both modes.

The SDK gRPC listener is disabled by default. Enabling `runtime.grpc.enabled`
adds an h2c ClusterIP port accepted only from the configured Envoy selector;
Envoy must authenticate the gRPC call and inject the same signed
`x-authguard-context` metadata. Do not expose that internal h2c port directly.
