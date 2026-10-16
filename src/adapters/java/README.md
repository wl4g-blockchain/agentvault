# AgentVault Java adapter

`AgentVault.payer(config).standard()` is a Spring
`ClientHttpRequestInterceptor` for payable outbound HTTP/MCP calls.
`SpringInterceptorPayeeMiddleware` is a servlet filter that protects selected
routes and MCP tools before a business controller runs.

Construct `AgentVault` with `GrpcVaultTransport`, `MqttVaultTransport`, or
`HttpVaultTransport`; changing transport does not change controllers or
Middleware configuration. Run `mvn test` from this directory.
