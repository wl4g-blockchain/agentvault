# AgentVault Go adapter

The Go SDK exposes one framework-neutral core and thin ecosystem adapters:

- `PayerMiddleware.Standard` installs a standard `http.RoundTripper` that handles
  402, approval wait, credential retry, and payment-response reporting.
- `PayeeMiddleware.Gin` returns Gin middleware for protected HTTP routes or a
  named MCP tool.

```go
wire, err := agentvault.NewMqttVaultTransport(mqttConfig) // or NewGrpcVaultTransport
if err != nil { return err }
vault := agentvault.New(wire)
core, err := vault.Payer(payerConfig)
if err != nil { return err }
client := core.Standard(nil).Client()

operation, err := agentvault.NewPayerRequestContext(runID, "realtime-news")
request = agentvault.WithPayment(request, operation)
response, err := client.Do(request)
```

The transport is selected once at startup; handlers and Middleware code are
unchanged between gRPC and MQTT. DTOs never carry a
caller principal or `client_id`, and no SDK accepts private keys or arbitrary
digests.

Run `go test ./...` from this directory.
