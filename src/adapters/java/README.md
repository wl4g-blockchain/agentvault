# AgentVault Java adapter

The Spring adapters target Spring Framework **7.0.9** and Jakarta Servlet **6.1**,
matching the Spring Boot **4.0.8** examples. Java 17 is the minimum runtime.

`AgentVault.payer(config).spring()` is a Spring
`ClientHttpRequestInterceptor` for payable outbound HTTP/MCP calls.
`SpringInterceptorPayeeMiddleware` is a servlet filter that protects selected
routes and MCP tools before a business controller runs.

Construct `AgentVault` with `GrpcVaultTransport` or `MqttVaultTransport`;
changing transport does not change controllers or
Middleware configuration. Run `mvn test` from this directory.

JDK, MVC, and WebFlux share one MCP/A2A message mapper. The payer follows its
protocol's declared metadata placement; MPP receivers accept both root `_meta`
and `params._meta`, as required by the
[MCP transport draft](https://paymentauth.org/draft-payment-transport-mcp-00.pdf).
Duplicate credentials are rejected, and successful paid calls require a matching receipt.
