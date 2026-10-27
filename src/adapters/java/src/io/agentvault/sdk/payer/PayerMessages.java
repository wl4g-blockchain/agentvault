package io.agentvault.sdk.payer;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;

import io.agentvault.sdk.model.A2a;
import io.agentvault.sdk.model.Payer.AuthorizedPayment;
import io.agentvault.sdk.model.Payer.PayerRequestContext;

import java.io.IOException;
import java.util.Map;

/** Native MCP/A2A carrier mapping shared by JDK, Spring MVC and WebFlux. */
final class PayerMessages {
    static final ObjectMapper JSON = new ObjectMapper();
    private static final System.Logger LOG = System.getLogger("io.agentvault.sdk");
    private final PayerMiddleware core;

    PayerMessages(PayerMiddleware core) {
        this.core = core;
    }

    record Call(JsonNode id, ObjectNode message, A2a.Call a2a) {}

    Call parse(String method, java.util.Map<String, String> headers, byte[] body) {
        if (!"POST".equalsIgnoreCase(method)
                || !isJson(PayerMiddleware.header(headers, "content-type"))) return null;
        try {
            var a2a = A2a.parse(body, core.config().maxChallengeBytes());
            if (a2a != null) return new Call(a2a.id(), a2a.wire(), a2a);
            JsonNode value = JSON.readTree(body);
            if (value == null
                    || !value.isObject()
                    || !"2.0".equals(value.path("jsonrpc").asText())
                    || !"tools/call".equals(value.path("method").asText())
                    || !value.path("params").isObject()
                    || !(value.path("id").isTextual() || value.path("id").isNumber())) return null;
            return new Call(value.get("id"), (ObjectNode) value, null);
        } catch (IOException error) {
            return null;
        }
    }

    byte[] credential(Call call, String key, JsonNode value) {
        ObjectNode message = call.message().deepCopy();
        ObjectNode params = (ObjectNode) message.path("params");
        JsonNode rootExisting = message.get("_meta");
        if (rootExisting != null && !rootExisting.isObject()) {
            throw new IllegalArgumentException("JSON-RPC root _meta must be an object");
        }
        JsonNode paramsExisting = params.get("_meta");
        if (paramsExisting != null && !paramsExisting.isObject()) {
            throw new IllegalArgumentException("MCP params._meta must be an object");
        }
        for (JsonNode existing : new JsonNode[] {rootExisting, paramsExisting}) {
            if (existing != null
                    && core.mcpRequestMetadataKeys().stream().anyMatch(existing::has)) {
                throw new IllegalArgumentException(
                        "MCP request already contains payment credentials");
            }
        }
        boolean atRoot = core.mcpRootRequestMetadataKeys().contains(key);
        ObjectNode carrier = atRoot ? message : params;
        JsonNode existing = atRoot ? rootExisting : paramsExisting;
        ObjectNode metadata =
                existing == null ? JSON.createObjectNode() : ((ObjectNode) existing).deepCopy();
        metadata.set(key, value);
        carrier.set("_meta", metadata);
        try {
            byte[] encoded = JSON.writeValueAsBytes(message);
            if (encoded.length > core.config().maxChallengeBytes()) {
                throw new IllegalArgumentException("paid MCP request exceeds configured limit");
            }
            return encoded;
        } catch (JsonProcessingException error) {
            throw new IllegalArgumentException("paid MCP request cannot be encoded", error);
        }
    }

    static boolean isJson(String value) {
        return value != null
                && value.toLowerCase(java.util.Locale.ROOT).startsWith("application/json");
    }

    static void event(String event, String runId, String intentId, Integer attemptNo) {
        LOG.log(
                System.Logger.Level.INFO,
                "{0} run_id={1} intent_id={2} attempt_no={3}",
                event,
                runId,
                intentId == null ? "" : intentId,
                attemptNo == null ? "" : attemptNo);
    }

    void report(
            PayerRequestContext context,
            AuthorizedPayment authorized,
            Call call,
            int status,
            Map<String, String> headers,
            byte[] body)
            throws IOException {
        if (status == 503 || status < 200 || status >= 300) return;
        if (!isJson(PayerMiddleware.header(headers, "content-type")))
            throw new IOException("paid JSON-RPC response must be application/json");
        if (body.length > core.config().maxChallengeBytes()) {
            throw new IOException("paid JSON-RPC response exceeds configured limit");
        }
        final JsonNode message;
        try {
            message = JSON.readTree(body);
        } catch (JsonProcessingException error) {
            throw new IOException("paid JSON-RPC response is not valid JSON", error);
        }
        if (!PayerMiddleware.boundMcpResponse(message, call.id(), true))
            throw new IOException("paid JSON-RPC response has no matching result");
        if (call.a2a() != null) {
            core.a2a().report(context, authorized, call.a2a(), message, headers);
            return;
        }
        JsonNode metadata = message.path("result").path("_meta");
        core.reportMcp(
                context, authorized, metadata.isObject() ? metadata : JSON.createObjectNode());
    }
}
