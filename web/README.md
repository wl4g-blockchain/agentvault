# AgentVault Dashboard

The dashboard is a same-origin operations client for AgentVault. It never asks
for, persists, or attaches bearer tokens. In the normal deployment AuthGuard
and Envoy establish the principal before forwarding `/api/v1`; in explicitly
disabled-auth deployments the same browser client works unchanged.

```bash
npm ci
npm run test
npm run build
```

The Vite development server proxies `/api/v1` to `AGENTVAULT_API_TARGET` and
the same-origin `/agentvault-management` health prefix to
`AGENTVAULT_MANAGEMENT_TARGET`. Production routing is owned by the deployment
gateway; the dashboard Nginx image only serves static assets.

Operational lists request and render 10 rows per page. Ledger conservation is
computed from a separately bounded, complete fact read rather than the visible
page. Client governance is read-only and shows the trusted identity binding
beside its associated Scenarios; Scenario IDs are the only configuration
identity and no external-application reference is stored. Scenario Wallet
membership uses a single-column, height-bounded,
scrolling multi-select dropdown so large Wallet catalogs cannot widen the form.
