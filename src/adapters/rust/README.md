# AgentVault Rust adapter

The Rust SDK keeps one transport-neutral payment core behind two ergonomic
integration points:

- `PayerMiddleware::standard()` returns a real `ReqwestPayerMiddleware`. Add
  a stable `PayerRequestContext` only to operations that may pay; ordinary
  requests pass through unchanged.
- `PayeeMiddleware::axum(routes)` returns an Axum/Tower layer. A route can
  protect an HTTP resource or one named MCP tool while free tools on the same
  endpoint continue normally.

```rust,ignore
let transport = MqttVaultTransport::new(mqtt_config)?; // or GrpcVaultTransport
let vault = AgentVault::new(transport);

let paid_http = vault.payer(payer_config)?.standard().client()?;
let response = paid_http
    .post(marketpulse_url)
    .with_payment(PayerRequestContext::for_operation(run_id, "realtime-news")?)
    .send()
    .await?;
```

For a payee, build the Axum layer once and keep the settled payment in request
extensions for the business handler:

```rust,ignore
let layer = vault.payee(PayeeConfig::default())?.axum(vec![
    AxumPayeeRoute::mcp_tool(
        "/mcp", "marketpulse.realtime_news", scenario_id,
        offer_id, "mcp://marketpulse/realtime-news", public_origin,
    ),
]);
let app = Router::new().route("/mcp", post(mcp)).layer(layer);
```

Available Vault transports are bounded HTTP, gRPC, and client-scoped MQTT.
Remote gRPC and MQTT require TLS; redirects are never followed with payment
credentials. Stable idempotency keys are created once per logical operation
and reused across approval waits, process retries, and delivery retries.

Run `cargo test --locked` from this directory.
