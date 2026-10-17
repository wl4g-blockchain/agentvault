package io.agentvault.e2e.agent;

import com.fasterxml.jackson.databind.ObjectMapper;
import io.agentvault.sdk.AgentVault;
import io.agentvault.sdk.config.Config.MqttConfig;
import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.transport.MqttVaultTransport;
import io.modelcontextprotocol.client.McpClient;
import io.modelcontextprotocol.client.McpSyncClient;
import io.modelcontextprotocol.spec.McpSchema.CallToolRequest;
import java.net.URI;
import java.time.Duration;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import org.springframework.ai.mcp.client.webflux.transport.WebClientStreamableHttpTransport;
import org.springframework.stereotype.Component;
import org.springframework.web.reactive.function.client.ClientRequest;
import org.springframework.web.reactive.function.client.ExchangeFilterFunction;
import org.springframework.web.reactive.function.client.WebClient;

/** Official MCP client with two remotely discovered MarketPulse tools. */
@Component
public final class MCPHelper implements AutoCloseable {
    private static final ObjectMapper JSON = new ObjectMapper();
    private static final String YESTERDAY_TOOL = "marketpulse.yesterday_news";
    private static final String REALTIME_TOOL = "marketpulse.realtime_news";

    public record ToolResult(byte[] body, String paymentIntentId) {}

    private static final class Operation {
        private final String runId;
        private final boolean paid;
        private final boolean failAfterSettlement;
        private String paymentIntentId;

        private Operation(String runId, boolean paid, boolean failAfterSettlement) {
            this.runId = runId;
            this.paid = paid;
            this.failAfterSettlement = failAfterSettlement;
        }
    }

    private final ThreadLocal<Operation> operation = new ThreadLocal<>();
    private final Map<String, Operation> activeOperations = new ConcurrentHashMap<>();
    private final MqttVaultTransport mqtt;
    private final McpSyncClient mcp;
    private final Map<String, Boolean> tools;

    public MCPHelper() {
        long timeoutSeconds = positive("AGENTVAULT_MQTT_TIMEOUT_SECONDS");
        MqttVaultTransport mqttTransport = new MqttVaultTransport(new MqttConfig(
                required("AGENTVAULT_MQTT_HOST"),
                Math.toIntExact(positive("AGENTVAULT_MQTT_PORT")),
                bool("AGENTVAULT_MQTT_TLS"),
                required("AGENTVAULT_MQTT_USERNAME"),
                required("AGENTVAULT_MQTT_PASSWORD"),
                "quantstream-java",
                required("QUANTSTREAM_CLIENT_ID"),
                Duration.ofSeconds(timeoutSeconds),
                2 * 1024 * 1024));
        McpSyncClient mcpClient = null;
        try {
            mqttTransport.connect();
            PayerConfig payerConfig = new PayerConfig(
                    required("QUANTSTREAM_SCENARIO_ID"),
                    Duration.ofSeconds(positive("AGENTVAULT_APPROVAL_TIMEOUT_SECONDS")),
                    Duration.ofSeconds(1),
                    1,
                    256 * 1024);
            var payer = new AgentVault(mqttTransport).payer(payerConfig).standard();
            URI endpoint = URI.create(required("MARKETPULSE_URL"));
            String origin = endpoint.getScheme() + "://" + endpoint.getAuthority();
            String path = endpoint.getRawPath();
            WebClient.Builder client = WebClient.builder()
                    .baseUrl(origin)
                    .filter(operationHeaders())
                    .filter(paymentEvidence())
                    .filter(payer.webClientFilter());
            var transport = WebClientStreamableHttpTransport.builder(client)
                    .endpoint(path == null || path.isBlank() ? "/mcp" : path)
                    .build();
            mcpClient = McpClient.sync(transport)
                    .requestTimeout(Duration.ofSeconds(timeoutSeconds))
                    .initializationTimeout(Duration.ofSeconds(timeoutSeconds))
                    .build();
            mcpClient.initialize();
            Map<String, Boolean> expected = Map.of(YESTERDAY_TOOL, false, REALTIME_TOOL, true);
            var discovered = mcpClient.listTools().tools().stream()
                    .map(tool -> tool.name())
                    .collect(java.util.stream.Collectors.toSet());
            if (!discovered.containsAll(expected.keySet())) {
                throw new IllegalStateException("MarketPulse did not publish both required MCP tools");
            }
            mqtt = mqttTransport;
            mcp = mcpClient;
            tools = expected;
        } catch (RuntimeException | Error error) {
            if (mcpClient != null) {
                try {
                    mcpClient.close();
                } catch (RuntimeException ignored) {
                    // Preserve the startup error; cleanup is best-effort.
                }
            }
            mqttTransport.close();
            throw error;
        }
    }

    public Map<String, Boolean> tools() {
        return tools;
    }

    public ToolResult yesterdayNews(String runId) throws Exception {
        return call(YESTERDAY_TOOL, runId, false);
    }

    public ToolResult realtimeNews(String runId, boolean failAfterSettlement) throws Exception {
        return call(REALTIME_TOOL, runId, failAfterSettlement);
    }

    private ToolResult call(String tool, String runId, boolean failAfterSettlement)
            throws Exception {
        Boolean paid = tools.get(tool);
        if (paid == null) throw new IllegalArgumentException("MarketPulse MCP tool is not registered: " + tool);
        Operation current = new Operation(runId, paid, failAfterSettlement);
        operation.set(current);
        activeOperations.put(runId, current);
        try {
            var result = mcp.callTool(new CallToolRequest(tool, Map.of()));
            if (Boolean.TRUE.equals(result.isError())) {
                throw new IllegalStateException("MarketPulse MCP tool returned an application error");
            }
            Object payload = result.structuredContent() == null ? result.content() : result.structuredContent();
            byte[] body = JSON.writeValueAsBytes(payload);
            if (body.length > 8 * 1024 * 1024) {
                throw new IllegalStateException("MarketPulse response exceeds configured limit");
            }
            if (paid && (current.paymentIntentId == null || current.paymentIntentId.isBlank())) {
                throw new IllegalStateException("paid response omitted AgentVault payment metadata");
            }
            return new ToolResult(body, current.paymentIntentId);
        } finally {
            activeOperations.remove(runId);
            operation.remove();
        }
    }

    private ExchangeFilterFunction operationHeaders() {
        return (request, next) -> {
            Operation current = operation.get();
            if (current == null) return next.exchange(request);
            ClientRequest.Builder enriched = ClientRequest.from(request)
                    .header("x-run-id", current.runId);
            if (current.failAfterSettlement) {
                enriched.header("x-e2e-fail-after-settlement", "once");
            }
            return next.exchange(enriched.build());
        };
    }

    private ExchangeFilterFunction paymentEvidence() {
        return (request, next) -> next.exchange(request).doOnNext(response -> {
            String runId = request.headers().getFirst("x-run-id");
            Operation current = runId == null ? null : activeOperations.get(runId);
            if (current != null) {
                current.paymentIntentId = response.headers().header("x-agentvault-payment-intent-id")
                        .stream().findFirst().orElse(current.paymentIntentId);
            }
        });
    }

    @Override
    public void close() {
        if (mcp != null) mcp.close();
        mqtt.close();
    }

    private static String required(String name) {
        String value = System.getenv(name);
        if (value == null || value.isBlank()) throw new IllegalStateException(name + " is required");
        return value;
    }

    private static long positive(String name) {
        long value = Long.parseLong(required(name));
        if (value <= 0) throw new IllegalStateException(name + " must be positive");
        return value;
    }

    private static boolean bool(String name) {
        String value = required(name).toLowerCase(java.util.Locale.ROOT);
        if (!value.equals("true") && !value.equals("false")) {
            throw new IllegalStateException(name + " must be boolean");
        }
        return Boolean.parseBoolean(value);
    }

}
