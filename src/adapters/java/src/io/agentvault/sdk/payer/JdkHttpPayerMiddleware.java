package io.agentvault.sdk.payer;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import io.agentvault.sdk.model.Payer.AuthorizedPayment;
import io.agentvault.sdk.model.Payer.PayerHttpRequest;
import io.agentvault.sdk.model.Payer.PayerHttpResponse;
import io.agentvault.sdk.model.Payer.PayerRequestContext;
import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.util.LinkedHashMap;
import java.util.Map;

/** Spring-independent JDK HttpClient payer wrapper with replayable byte bodies. */
public final class JdkHttpPayerMiddleware {
    private static final ObjectMapper JSON = new ObjectMapper();
    public record Result(HttpResponse<byte[]> response, AuthorizedPayment payment) {}
    private record McpCall(JsonNode id, ObjectNode message) {}

    private final PayerMiddleware core;
    private final HttpClient client;

    public JdkHttpPayerMiddleware(PayerMiddleware core) {
        this(core, HttpClient.newBuilder().followRedirects(HttpClient.Redirect.NEVER).build());
    }

    public JdkHttpPayerMiddleware(PayerMiddleware core, HttpClient client) {
        this.core = java.util.Objects.requireNonNull(core, "core");
        this.client = java.util.Objects.requireNonNull(client, "client");
        if (client.followRedirects() != HttpClient.Redirect.NEVER) {
            throw new IllegalArgumentException("payer HTTP client must disable redirects");
        }
    }

    public Result send(HttpRequest request, byte[] replayableBody, PayerRequestContext context)
            throws IOException, InterruptedException {
        java.util.Objects.requireNonNull(request, "request");
        byte[] body = replayableBody == null ? new byte[0] : replayableBody.clone();
        if (body.length > core.config().maxChallengeBytes()) {
            throw new IllegalArgumentException("paid request body exceeds configured limit");
        }
        McpCall call = mcpCall(request, body);
        HttpRequest firstRequest = copy(request, body, null);
        HttpResponse<byte[]> first = client.send(firstRequest, HttpResponse.BodyHandlers.ofByteArray());
        if (context == null) return new Result(first, null);
        URI target = PayerMiddleware.safeUri(request.uri().toString());
        if (first.statusCode() != 402) {
            Result paid = handleMcpChallenge(request, body, call, first, target, context);
            return paid == null ? new Result(first, null) : paid;
        }
        AuthorizedPayment authorized = core.authorize(
                new PayerHttpRequest(
                        request.method(), target.toString(), PayerMiddleware.origin(target),
                        headers(request.headers().map()), body, false),
                new PayerHttpResponse(first.statusCode(), headers(first.headers().map()), first.body()),
                context);
        Map.Entry<String, String> delivery = core.credentialHeader(
                authorized, PayerMiddleware.origin(target), headers(request.headers().map()));
        HttpRequest paid = copy(request, body, delivery);
        final HttpResponse<byte[]> response;
        try {
            response = core.deliver(
                    context, authorized,
                    ignored -> {
                        try {
                            return client.send(paid, HttpResponse.BodyHandlers.ofByteArray());
                        } catch (InterruptedException error) {
                            Thread.currentThread().interrupt();
                            throw new InterruptedDelivery(error);
                        }
                    },
                    new PayerMiddleware.DeliveryResponse<HttpResponse<byte[]>, IOException>() {
                        @Override public PayerHttpResponse view(HttpResponse<byte[]> value) {
                            return new PayerHttpResponse(
                                    value.statusCode(), headers(value.headers().map()), new byte[0]);
                        }

                        @Override public void discard(HttpResponse<byte[]> value) {
                            // Byte-array responses are already fully consumed.
                        }
                    });
        } catch (InterruptedDelivery error) {
            throw error.interrupted;
        }
        return new Result(response, authorized);
    }

    private Result handleMcpChallenge(
            HttpRequest request,
            byte[] body,
            McpCall call,
            HttpResponse<byte[]> first,
            URI target,
            PayerRequestContext context) throws IOException, InterruptedException {
        if (call == null || !isJson(first.headers().firstValue("content-type").orElse(null))) return null;
        if (first.body().length > core.config().maxChallengeBytes()) {
            throw new IOException("payment challenge exceeds configured limit");
        }
        final JsonNode message;
        try {
            message = JSON.readTree(first.body());
        } catch (IOException error) {
            return null;
        }
        if (!PayerMiddleware.boundMcpResponse(message, call.id(), false)) return null;
        AuthorizedPayment authorized = core.authorizeMcp(
                new PayerHttpRequest(
                        request.method(), target.toString(), PayerMiddleware.origin(target),
                        headers(request.headers().map()), body, false),
                message,
                context,
                headers(first.headers().map()));
        if (authorized == null) return null;
        var metadata = core.mcpCredential(authorized, PayerMiddleware.origin(target));
        byte[] paidBody = attachMcpCredential(call, metadata.key(), metadata.value());
        HttpRequest paid = copy(request, paidBody, null);
        final HttpResponse<byte[]> response;
        try {
            response = core.deliver(
                    context,
                    authorized,
                    ignored -> {
                        try {
                            return client.send(paid, HttpResponse.BodyHandlers.ofByteArray());
                        } catch (InterruptedException error) {
                            Thread.currentThread().interrupt();
                            throw new InterruptedDelivery(error);
                        }
                    },
                    new PayerMiddleware.DeliveryResponse<HttpResponse<byte[]>, IOException>() {
                        @Override public PayerHttpResponse view(HttpResponse<byte[]> value) {
                            return new PayerHttpResponse(
                                    value.statusCode(), headers(value.headers().map()), new byte[0]);
                        }

                        @Override public void discard(HttpResponse<byte[]> ignored) {
                            // Byte-array responses are already fully consumed.
                        }
                    },
                    value -> reportMcp(context, authorized, call, value));
        } catch (InterruptedDelivery error) {
            throw error.interrupted;
        }
        return new Result(response, authorized);
    }

    private void reportMcp(
            PayerRequestContext context,
            AuthorizedPayment authorized,
            McpCall call,
            HttpResponse<byte[]> response) throws IOException {
        if (response.statusCode() == 503 || response.statusCode() < 200 || response.statusCode() >= 300) return;
        if (!isJson(response.headers().firstValue("content-type").orElse(null))) {
            throw new IOException("paid MCP response must be application/json");
        }
        if (response.body().length > core.config().maxChallengeBytes()) {
            throw new IOException("paid MCP response exceeds configured limit");
        }
        final JsonNode message;
        try {
            message = JSON.readTree(response.body());
        } catch (JsonProcessingException error) {
            throw new IOException("paid MCP response is not valid JSON", error);
        }
        if (!PayerMiddleware.boundMcpResponse(message, call.id(), true)) {
            throw new IOException("paid MCP response has no matching JSON-RPC result");
        }
        JsonNode metadata = message.path("result").path("_meta");
        core.reportMcp(context, authorized,
                metadata.isObject() ? metadata : JSON.createObjectNode());
    }

    private McpCall mcpCall(HttpRequest request, byte[] body) {
        if (!"POST".equalsIgnoreCase(request.method())
                || !isJson(request.headers().firstValue("content-type").orElse(null))) return null;
        try {
            JsonNode value = JSON.readTree(body);
            if (value == null || !value.isObject() || !"2.0".equals(value.path("jsonrpc").asText())
                    || !"tools/call".equals(value.path("method").asText())
                    || !value.path("params").isObject()
                    || !(value.path("id").isTextual() || value.path("id").isNumber())) return null;
            return new McpCall(value.get("id"), (ObjectNode) value);
        } catch (IOException error) {
            return null;
        }
    }

    private byte[] attachMcpCredential(McpCall call, String key, JsonNode value) {
        ObjectNode message = call.message().deepCopy();
        ObjectNode params = (ObjectNode) message.path("params");
        JsonNode rootExisting = message.get("_meta");
        if (rootExisting != null && !rootExisting.isObject()) {
            throw new IllegalArgumentException("JSON-RPC root _meta must be an object");
        }
        if (rootExisting != null && core.mcpRootRequestMetadataKeys().stream().anyMatch(rootExisting::has)) {
            throw new IllegalArgumentException("MCP request already contains root payment credentials");
        }
        JsonNode existing = params.get("_meta");
        if (existing != null && !existing.isObject()) {
            throw new IllegalArgumentException("MCP params._meta must be an object");
        }
        ObjectNode metadata = existing == null
                ? JSON.createObjectNode() : ((ObjectNode) existing).deepCopy();
        if (core.mcpRequestMetadataKeys().stream().anyMatch(metadata::has)) {
            throw new IllegalArgumentException("MCP request already contains payment credentials");
        }
        metadata.set(key, value);
        params.set("_meta", metadata);
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

    private static boolean isJson(String value) {
        return value != null && value.toLowerCase(java.util.Locale.ROOT).startsWith("application/json");
    }

    private HttpRequest copy(HttpRequest source, byte[] body, Map.Entry<String, String> credential) {
        HttpRequest.Builder builder = HttpRequest.newBuilder(source.uri())
                .method(source.method(), body.length == 0
                        ? HttpRequest.BodyPublishers.noBody()
                        : HttpRequest.BodyPublishers.ofByteArray(body))
                .expectContinue(source.expectContinue());
        source.timeout().ifPresent(builder::timeout);
        source.version().ifPresent(builder::version);
        source.headers().map().forEach((name, values) -> {
            if (!name.equalsIgnoreCase("content-length")) {
                values.forEach(value -> builder.header(name, value));
            }
        });
        if (credential != null) builder.header(credential.getKey(), credential.getValue());
        return builder.build();
    }

    private static Map<String, String> headers(Map<String, java.util.List<String>> values) {
        var headers = new LinkedHashMap<String, String>();
        values.forEach((name, entries) -> headers.put(name, String.join(", ", entries)));
        return headers;
    }

    private static final class InterruptedDelivery extends RuntimeException {
        private final InterruptedException interrupted;

        private InterruptedDelivery(InterruptedException interrupted) {
            super(interrupted);
            this.interrupted = interrupted;
        }
    }
}
