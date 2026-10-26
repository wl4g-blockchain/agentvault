package io.agentvault.e2e.handler;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import io.agentvault.e2e.storage.FeedSqliteRepository;
import io.agentvault.sdk.model.Payee.SettledIncomingPayment;
import io.agentvault.sdk.payee.SpringInterceptorPayeeMiddleware;
import jakarta.servlet.http.HttpServletRequest;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.HexFormat;
import java.util.List;
import java.util.Map;
import org.springframework.http.MediaType;
import org.springframework.http.ResponseEntity;
import org.springframework.stereotype.Service;

@Service
public final class FeedHandler {
    private static final ObjectMapper JSON = new ObjectMapper();
    private final FeedSqliteRepository storage;
    private final boolean allowFailureInjection;

    public FeedHandler(FeedSqliteRepository storage) {
        this.storage = storage;
        this.allowFailureInjection = "true".equals(System.getenv("MARKETPULSE_E2E_ALLOW_FAILURE_INJECTION"));
    }

    public ResponseEntity<byte[]> health() {
        return response(200, Map.of("status", "ready", "service", "marketpulse-java"));
    }

    public ResponseEntity<byte[]> metrics() {
        return ResponseEntity.ok()
                .contentType(MediaType.parseMediaType("text/plain; version=0.0.4"))
                .body(("# HELP marketpulse_up MarketPulse process readiness.\n"
                        + "# TYPE marketpulse_up gauge\nmarketpulse_up 1\n").getBytes(StandardCharsets.UTF_8));
    }

    public ResponseEntity<byte[]> mcp(HttpServletRequest servletRequest, byte[] body) {
        JsonNode request;
        try {
            request = JSON.readTree(body);
        } catch (Exception error) {
            return response(400, rpcError(null, -32600, "invalid request"));
        }
        JsonNode id = request.get("id");
        if (!"2.0".equals(request.path("jsonrpc").asText())) {
            return response(400, rpcError(id, -32600, "invalid request"));
        }
        return switch (request.path("method").asText()) {
            case "initialize" -> response(200, rpcResult(id, Map.of(
                    "protocolVersion", request.path("params").path("protocolVersion").asText("2025-11-25"),
                    "capabilities", Map.of("tools", Map.of("listChanged", false)),
                    "serverInfo", Map.of("name", "marketpulse-feed-java", "version", "0.1.0"))));
            case "notifications/initialized" -> ResponseEntity.status(202).build();
            case "tools/list" -> response(200, rpcResult(id, Map.of("tools", tools())));
            case "tools/call" -> callTool(servletRequest, body, id, request.path("params"));
            default -> response(404, rpcError(id, -32601, "method not found"));
        };
    }

    private ResponseEntity<byte[]> callTool(HttpServletRequest request, byte[] body, JsonNode id, JsonNode params) {
        String name = params.path("name").asText();
        if ("marketpulse.yesterday_news".equals(name)) {
            return response(200, toolResult(id, storage.news("yesterday", 20), false, null));
        }
        if (!"marketpulse.realtime_news".equals(name)) {
            return response(404, rpcError(id, -32602, "unknown tool"));
        }
        String runId = request.getHeader("x-run-id");
        if (runId == null || runId.isBlank()) {
            return response(400, rpcError(id, -32602, "x-run-id is required"));
        }
        String requestHash = requestHash(body);
        FeedSqliteRepository.DeliveryState delivery;
        try {
            delivery = storage.delivery(runId, requestHash);
        } catch (IllegalStateException error) {
            return response(409, rpcError(id, -32009, error.getMessage()));
        }
        if (delivery.response() != null) {
            return raw(200, delivery.response());
        }
        String intentId = delivery.intentId();
        if (intentId == null) {
            Object value = request.getAttribute(SpringInterceptorPayeeMiddleware.PAYMENT_ATTRIBUTE);
            if (!(value instanceof SettledIncomingPayment payment)) {
                return response(402, rpcError(id, -32005, "settled payment is required"));
            }
            if (!runId.equals(payment.intent().runId())) {
                throw new IllegalStateException("settlement is not bound to the workflow run");
            }
            storage.recordSettled(runId, requestHash, payment.intent().id());
            intentId = payment.intent().id();
        }
        if (allowFailureInjection && "once".equals(request.getHeader("x-e2e-fail-after-settlement"))
                && storage.recordFailure(runId, requestHash)) {
            return response(503, rpcError(id, -32004, "injected delivery failure after settlement"));
        }
        Object payload = toolResult(id, storage.news("realtime", 20), true, intentId);
        String encoded = encode(payload);
        storage.recordDelivered(runId, requestHash, encoded);
        return raw(200, encoded);
    }

    private static List<Map<String, Object>> tools() {
        Map<String, Object> schema = Map.of("type", "object", "additionalProperties", false);
        return List.of(
                Map.of("name", "marketpulse.yesterday_news", "description",
                        "Free previous-day geopolitical and tariff intelligence", "inputSchema", schema),
                Map.of("name", "marketpulse.realtime_news", "description",
                        "Paid real-time geopolitical and tariff intelligence via x402", "inputSchema", schema));
    }

    private static Map<String, Object> toolResult(JsonNode id, Object news, boolean paid, String intentId) {
        ObjectNode structured = JSON.createObjectNode();
        structured.set("news", JSON.valueToTree(news));
        structured.put("paid", paid);
        if (intentId == null) structured.putNull("intentId"); else structured.put("intentId", intentId);
        return rpcResult(id, Map.of(
                "content", List.of(Map.of("type", "text", "text", structured.toString())),
                "structuredContent", structured,
                "isError", false));
    }

    private static Map<String, Object> rpcResult(JsonNode id, Object result) {
        return Map.of("jsonrpc", "2.0", "id", id == null ? JSON.nullNode() : id, "result", result);
    }

    private static Map<String, Object> rpcError(JsonNode id, int code, String message) {
        return Map.of("jsonrpc", "2.0", "id", id == null ? JSON.nullNode() : id,
                "error", Map.of("code", code, "message", message));
    }

    private static String requestHash(byte[] body) {
        try {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            digest.update("marketpulse.mcp.request.v1\0".getBytes(StandardCharsets.UTF_8));
            return HexFormat.of().formatHex(digest.digest(body));
        } catch (java.security.NoSuchAlgorithmException error) {
            throw new IllegalStateException(error);
        }
    }

    private static ResponseEntity<byte[]> response(int status, Object value) {
        return ResponseEntity.status(status).contentType(MediaType.APPLICATION_JSON).body(encode(value).getBytes(StandardCharsets.UTF_8));
    }

    private static ResponseEntity<byte[]> raw(int status, String value) {
        return ResponseEntity.status(status).contentType(MediaType.APPLICATION_JSON).body(value.getBytes(StandardCharsets.UTF_8));
    }

    private static String encode(Object value) {
        try {
            return JSON.writeValueAsString(value);
        } catch (com.fasterxml.jackson.core.JsonProcessingException error) {
            throw new IllegalStateException(error);
        }
    }
}
