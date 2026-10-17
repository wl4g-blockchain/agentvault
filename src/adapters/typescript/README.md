# AgentVault TypeScript adapter

`vault.payer(config).standard()` returns a `FetchPayerMiddleware` over the platform
Fetch API. Pass a stable `PayerRequestContext` only for payable operations.
`vault.payee(config).express(routes).handlers()` installs the corresponding inbound
Offer/Challenge/Verify/Settle flow in Express.

Choose `GrpcVaultTransport`, `MqttVaultTransport`, or `HttpVaultTransport` once
at startup. The Middleware API and business handlers do not change.

Run `npm ci && npm run build && npm test` from this directory.
