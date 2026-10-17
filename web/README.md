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
