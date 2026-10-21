package io.agentvault.sdk.payee;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Payee.Authorized;
import io.agentvault.sdk.model.Payee.PayeeOperation;
import io.agentvault.sdk.model.Payee.PaymentRequired;
import io.agentvault.sdk.model.Payee.ProtectionOutcome;
import io.agentvault.sdk.payee.PayeeMiddleware.InvalidPaymentException;
import io.agentvault.sdk.protocol.PaymentProtocol;
import io.agentvault.sdk.transport.VaultTransport.ApiException;
import jakarta.servlet.FilterChain;
import jakarta.servlet.ReadListener;
import jakarta.servlet.ServletException;
import jakarta.servlet.ServletInputStream;
import jakarta.servlet.ServletOutputStream;
import jakarta.servlet.WriteListener;
import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletRequestWrapper;
import jakarta.servlet.http.HttpServletResponse;
import jakarta.servlet.http.HttpServletResponseWrapper;
import java.io.BufferedReader;
import java.io.ByteArrayInputStream;
import java.io.ByteArrayOutputStream;
import java.io.IOException;
import java.io.InputStreamReader;
import java.io.OutputStreamWriter;
import java.io.PrintWriter;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.ArrayList;
import java.util.Comparator;
import java.util.LinkedHashMap;
import java.util.List;
import org.springframework.web.filter.OncePerRequestFilter;

/** Spring Servlet payee adapter; register once as a Filter bean. */
public final class SpringInterceptorPayeeMiddleware extends OncePerRequestFilter {
    private static final System.Logger LOG = System.getLogger("io.agentvault.sdk");
    public static final String PAYMENT_ATTRIBUTE =
            SpringInterceptorPayeeMiddleware.class.getName() + ".PAYMENT";
    private static final ObjectMapper JSON = new ObjectMapper();

    public record Route(
            String method,
            String path,
            String scenarioId,
            String offerId,
            String resource,
            String origin,
            String protocol,
            String mcpTool) {
        public static Route mcpTool(
                String path,
                String tool,
                String scenarioId,
                String offerId,
                String resource,
                String origin) {
            return mcpToolWithProtocol(
                    path, tool, scenarioId, offerId, resource, origin, "x402");
        }

        public static Route mcpToolWithProtocol(
                String path,
                String tool,
                String scenarioId,
                String offerId,
                String resource,
                String origin,
                String protocol) {
            return new Route("POST", path, scenarioId, offerId, resource, origin, protocol, tool);
        }
    }

    private record McpCall(JsonNode id, ObjectNode metadata, byte[] canonicalBody) {
        @Override public byte[] canonicalBody() { return canonicalBody.clone(); }
    }

    private final PayeeMiddleware core;
    private final List<Route> routes;
    private final int maxBodyBytes;

    public SpringInterceptorPayeeMiddleware(PayeeMiddleware core, List<Route> routes, int maxBodyBytes) {
        this.core = java.util.Objects.requireNonNull(core, "core");
        this.routes = List.copyOf(routes);
        if (maxBodyBytes <= 0) throw new IllegalArgumentException("maxBodyBytes must be positive");
        this.maxBodyBytes = maxBodyBytes;
    }

    @Override protected void doFilterInternal(
            HttpServletRequest request,
            HttpServletResponse response,
            FilterChain chain) throws ServletException, IOException {
        Route route = routes.stream()
                .filter(value -> value.method().equalsIgnoreCase(request.getMethod())
                        && value.path().equals(request.getRequestURI()))
                .findFirst()
                .orElse(null);
        if (route == null) {
            chain.doFilter(request, response);
            return;
        }
        McpCall call = null;
        try {
            byte[] body = request.getInputStream().readNBytes(maxBodyBytes + 1);
            if (body.length > maxBodyBytes) {
                error(response, 413, "REQUEST_TOO_LARGE", "protected body exceeds limit");
                return;
            }
            var replayable = new ReplayableRequest(request, body);
            call = route.mcpTool() == null ? null : mcpCall(body, route.mcpTool());
            if (route.mcpTool() != null && call == null) {
                chain.doFilter(replayable, response);
                return;
            }
            if (call != null) {
                replayable = new ReplayableRequest(request, call.canonicalBody());
            }
            var requestHeaders = headers(request);
            String runId = request.getHeader("x-run-id");
            if (runId == null || runId.isBlank()) {
                error(response, 400, "PAYMENT_CONTEXT_REQUIRED", "x-run-id is required");
                return;
            }
            PaymentProtocol protocol = core.protocol(route.protocol());
            JsonNode httpCredential = core.presentedCredential(route.protocol(), requestHeaders);
            if (call != null && httpCredential != null) {
                throw new IllegalArgumentException(
                        "MCP payment credentials must use protocol metadata, not HTTP headers");
            }
            JsonNode credential = call == null
                    ? httpCredential
                    : core.presentedMcpCredential(route.protocol(), call.metadata());
            byte[] requestBody = call == null ? body : call.canonicalBody();
            ProtectionOutcome outcome = core.protect(new PayeeOperation(
                    route.scenarioId(), runId, route.offerId(), route.resource(), route.origin(),
                    route.protocol(), hash(request.getMethod(), request.getRequestURI(), requestBody), credential));
            if (outcome instanceof PaymentRequired required) {
                if (call == null) sendHttpChallenge(response, protocol, required.challenge());
                else sendMcpChallenge(response, call, required.challenge(), false);
                return;
            }
            Authorized authorized = (Authorized) outcome;
            replayable.setAttribute(PAYMENT_ATTRIBUTE, authorized.payment());
            if (call == null) {
                var receipt = protocol.receipt(authorized.payment());
                chain.doFilter(replayable, new ProtectedReceiptResponse(
                        response, receipt.headers(), core.merchantEvidenceHeaderNames()));
            } else {
                PaymentProtocol.McpMetadata receipt =
                        core.mcpReceipt(authorized.challenge(), authorized.payment());
                var buffered = new BufferedResponse(response, maxBodyBytes);
                chain.doFilter(replayable, buffered);
                buffered.commitMcp(call, receipt, core);
            }
            if (response.getStatus() >= 200 && response.getStatus() < 300) {
                LOG.log(System.Logger.Level.INFO,
                        "agentvault.sdk.payee.resource_released run_id={0} intent_id={1}",
                        runId, authorized.payment().intent().id());
            }
        } catch (InvalidPaymentException error) {
            if (call == null) {
                sendHttpChallenge(response, core.protocol(error.challenge().protocol()), error.challenge());
            } else {
                sendMcpChallenge(response, call, error.challenge(), true);
            }
        } catch (ApiException error) {
            error(response, error.status(), error.code(), error.getMessage());
        } catch (IllegalArgumentException error) {
            error(response, 400, "PAYMENT_INVALID", error.getMessage());
        } catch (SecurityException error) {
            error(response, 402, "PAYMENT_INVALID", error.getMessage());
        } catch (RuntimeException error) {
            error(response, 502, "AGENTVAULT_UNAVAILABLE", error.getMessage());
        }
    }

    private void sendHttpChallenge(
            HttpServletResponse response,
            PaymentProtocol protocol,
            PaymentChallenge challenge) throws IOException {
        PaymentProtocol.HttpResponse wire = protocol.challenge(challenge);
        response.reset();
        response.setStatus(402);
        response.setContentType(wire.contentType() == null ? "application/json" : wire.contentType());
        response.setHeader("payment-protocol", challenge.protocol());
        response.setHeader("payment-expires-at", Long.toString(challenge.expiresAt()));
        wire.headers().forEach(response::setHeader);
        response.getOutputStream().write(wire.body());
    }

    private void sendMcpChallenge(
            HttpServletResponse response,
            McpCall call,
            PaymentChallenge challenge,
            boolean rejected) throws IOException {
        PaymentProtocol.McpPaymentRequired wire =
                core.mcpPaymentRequired(challenge, rejected);
        ObjectNode envelope = JSON.createObjectNode();
        envelope.put("jsonrpc", "2.0");
        envelope.set("id", call.id());
        if (wire.result() != null) envelope.set("result", wire.result());
        else envelope.set("error", wire.error());
        byte[] body = JSON.writeValueAsBytes(envelope);
        if (body.length > maxBodyBytes) {
            throw new IllegalArgumentException("MCP payment challenge exceeds configured limit");
        }
        response.reset();
        response.setStatus(200);
        response.setContentType("application/json");
        response.setHeader("payment-protocol", challenge.protocol());
        response.setHeader("payment-expires-at", Long.toString(challenge.expiresAt()));
        response.setContentLength(body.length);
        response.getOutputStream().write(body);
    }

    private McpCall mcpCall(byte[] body, String expected) {
        try {
            JsonNode value = JSON.readTree(body);
            if (value == null || !value.isObject() || !"2.0".equals(value.path("jsonrpc").asText())
                    || !"tools/call".equals(value.path("method").asText())
                    || !value.path("params").isObject()
                    || !expected.equals(value.path("params").path("name").asText())) return null;
            if (!(value.path("id").isTextual() || value.path("id").isNumber())) {
                throw new IllegalArgumentException(
                        "paid MCP tools/call requires a string or numeric JSON-RPC id");
            }
            JsonNode existing = value.path("params").get("_meta");
            if (existing != null && !existing.isObject()) {
                throw new IllegalArgumentException("MCP params._meta must be an object");
            }
            ObjectNode metadata = existing == null
                    ? JSON.createObjectNode() : ((ObjectNode) existing).deepCopy();
            JsonNode rootExisting = value.get("_meta");
            if (rootExisting != null && !rootExisting.isObject()) {
                throw new IllegalArgumentException("JSON-RPC root _meta must be an object");
            }
            if (rootExisting != null) {
                for (String key : core.mcpRootRequestMetadataKeys()) {
                    if (!rootExisting.has(key)) continue;
                    if (metadata.has(key)) {
                        throw new IllegalArgumentException("duplicate MCP payment credential metadata");
                    }
                    metadata.set(key, rootExisting.get(key));
                }
            }
            ObjectNode canonical = ((ObjectNode) value).deepCopy();
            ObjectNode canonicalParams = (ObjectNode) canonical.path("params");
            JsonNode canonicalMetaValue = canonicalParams.get("_meta");
            if (canonicalMetaValue != null) {
                ObjectNode canonicalMeta = (ObjectNode) canonicalMetaValue;
                core.mcpRequestMetadataKeys().forEach(canonicalMeta::remove);
                if (canonicalMeta.isEmpty()) canonicalParams.remove("_meta");
            }
            JsonNode canonicalRootValue = canonical.get("_meta");
            if (canonicalRootValue != null) {
                ObjectNode canonicalRoot = (ObjectNode) canonicalRootValue;
                core.mcpRootRequestMetadataKeys().forEach(canonicalRoot::remove);
                if (canonicalRoot.isEmpty()) canonical.remove("_meta");
            }
            return new McpCall(value.get("id"), metadata,
                    canonicalJson(canonical).getBytes(StandardCharsets.UTF_8));
        } catch (IOException error) {
            return null;
        }
    }

    private java.util.Map<String, String> headers(HttpServletRequest request) {
        var headers = new LinkedHashMap<String, String>();
        var paymentHeaders = core.credentialHeaderNames();
        java.util.Collections.list(request.getHeaderNames()).forEach(name -> {
            var values = java.util.Collections.list(request.getHeaders(name));
            if (paymentHeaders.stream().anyMatch(name::equalsIgnoreCase) && values.size() != 1) {
                throw new IllegalArgumentException(
                        "duplicate payment request header is not allowed: " + name);
            }
            headers.put(name, String.join(", ", values));
        });
        return headers;
    }

    private static String hash(String method, String path, byte[] body) {
        try {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            digest.update("agentvault.payee.http.v1\0".getBytes(StandardCharsets.UTF_8));
            digest.update(method.toUpperCase(java.util.Locale.ROOT).getBytes(StandardCharsets.UTF_8));
            digest.update((byte) 0);
            digest.update(path.getBytes(StandardCharsets.UTF_8));
            digest.update((byte) 0);
            digest.update(body);
            return java.util.HexFormat.of().formatHex(digest.digest());
        } catch (java.security.NoSuchAlgorithmException error) {
            throw new IllegalStateException(error);
        }
    }

    private static String canonicalJson(JsonNode value) {
        try {
            if (value.isObject()) {
                var fields = new ArrayList<java.util.Map.Entry<String, JsonNode>>();
                value.fields().forEachRemaining(fields::add);
                fields.sort(Comparator.comparing(java.util.Map.Entry::getKey));
                var result = new StringBuilder("{");
                for (int index = 0; index < fields.size(); index++) {
                    if (index > 0) result.append(',');
                    var field = fields.get(index);
                    result.append(JSON.writeValueAsString(field.getKey()))
                            .append(':').append(canonicalJson(field.getValue()));
                }
                return result.append('}').toString();
            }
            if (value.isArray()) {
                var result = new StringBuilder("[");
                for (int index = 0; index < value.size(); index++) {
                    if (index > 0) result.append(',');
                    result.append(canonicalJson(value.get(index)));
                }
                return result.append(']').toString();
            }
            return JSON.writeValueAsString(value);
        } catch (JsonProcessingException error) {
            throw new IllegalArgumentException("MCP message contains an invalid JSON value", error);
        }
    }

    private static void error(
            HttpServletResponse response,
            int status,
            String code,
            String message) throws IOException {
        if (response.isCommitted()) throw new IOException("cannot replace a committed payment response");
        response.reset();
        response.setStatus(status);
        response.setContentType("application/json");
        JSON.writeValue(response.getOutputStream(), java.util.Map.of(
                "error", java.util.Map.of("code", code, "message", message == null ? code : message)));
    }

    private static final class BufferedResponse extends HttpServletResponseWrapper {
        private final int limit;
        private final ByteArrayOutputStream body = new ByteArrayOutputStream();
        private ServletOutputStream stream;
        private PrintWriter writer;

        private BufferedResponse(HttpServletResponse response, int limit) {
            super(response);
            this.limit = limit;
        }

        @Override public ServletOutputStream getOutputStream() {
            if (writer != null) throw new IllegalStateException("getWriter() was already called");
            return outputStream();
        }

        private ServletOutputStream outputStream() {
            if (stream == null) {
                stream = new ServletOutputStream() {
                    @Override public boolean isReady() { return true; }
                    @Override public void setWriteListener(WriteListener listener) {
                        throw new UnsupportedOperationException("SSE/async MCP responses are unsupported");
                    }
                    @Override public void write(int value) throws IOException {
                        ensureCapacity(1);
                        body.write(value);
                    }
                    @Override public void write(byte[] value, int offset, int length) throws IOException {
                        ensureCapacity(length);
                        body.write(value, offset, length);
                    }
                    private void ensureCapacity(int added) throws IOException {
                        if ((long) body.size() + added > limit) {
                            throw new IOException("paid MCP response exceeds configured limit");
                        }
                    }
                };
            }
            return stream;
        }

        @Override public PrintWriter getWriter() throws IOException {
            if (stream != null) throw new IllegalStateException("getOutputStream() was already called");
            if (writer == null) {
                writer = new PrintWriter(new OutputStreamWriter(
                        outputStream(), getCharacterEncoding()), false);
            }
            return writer;
        }

        @Override public void flushBuffer() throws IOException {
            if (writer != null) writer.flush();
        }

        private void commitMcp(
                McpCall call,
                PaymentProtocol.McpMetadata receipt,
                PayeeMiddleware core) throws IOException {
            if (writer != null) writer.flush();
            HttpServletResponse delegate = (HttpServletResponse) getResponse();
            if (delegate.getContentType() == null
                    || !delegate.getContentType().toLowerCase(java.util.Locale.ROOT)
                            .startsWith("application/json")) {
                throw new IllegalStateException(
                        "paid MCP handler must return a bounded application/json response");
            }
            var headers = new LinkedHashMap<String, String>();
            for (String name : delegate.getHeaderNames()) headers.put(name, delegate.getHeader(name));
            if (core.merchantEvidenceHeaderNames().stream()
                    .anyMatch(owned -> headers.keySet().stream().anyMatch(owned::equalsIgnoreCase))) {
                throw new IllegalStateException(
                        "paid MCP handler attempted to supply settlement evidence");
            }
            final JsonNode message;
            try {
                message = JSON.readTree(body.toByteArray());
            } catch (JsonProcessingException error) {
                throw new IllegalStateException("paid MCP handler returned invalid JSON", error);
            }
            if (message == null || !message.isObject() || !call.id().equals(message.get("id"))) {
                throw new IllegalStateException(
                        "paid MCP response id does not match its request");
            }
            JsonNode rawResult = message.get("result");
            if (rawResult != null && !rawResult.isObject()) {
                throw new IllegalStateException("paid MCP handler returned an invalid JSON-RPC result");
            }
            ObjectNode result = rawResult == null ? null : (ObjectNode) rawResult;
            JsonNode existing = result == null ? null : result.get("_meta");
            if (existing != null && !existing.isObject()) {
                throw new IllegalStateException("paid MCP result._meta must be an object");
            }
            ObjectNode metadata = existing == null
                    ? JSON.createObjectNode() : ((ObjectNode) existing).deepCopy();
            if (result != null
                    && (core.mcpMerchantEvidence(metadata) != null || metadata.has(receipt.key()))) {
                throw new IllegalStateException(
                        "paid MCP handler attempted to supply settlement metadata");
            }
            if (delegate.getStatus() < 200 || delegate.getStatus() >= 300) {
                if (result == null && !message.path("error").isObject()) {
                    throw new IllegalStateException(
                            "paid MCP handler must return a JSON-RPC result or error");
                }
                byte[] encoded = body.toByteArray();
                delegate.setContentType("application/json");
                delegate.setContentLength(encoded.length);
                delegate.getOutputStream().write(encoded);
                delegate.flushBuffer();
                return;
            }
            if (result == null) {
                throw new IllegalStateException(
                        "paid MCP handler must return a matching JSON-RPC result");
            }
            metadata.set(receipt.key(), receipt.value());
            result.set("_meta", metadata);
            byte[] encoded = JSON.writeValueAsBytes(message);
            if (encoded.length > limit) {
                throw new IllegalStateException("paid MCP response exceeds configured limit");
            }
            delegate.setContentType("application/json");
            delegate.setContentLength(encoded.length);
            delegate.getOutputStream().write(encoded);
            delegate.flushBuffer();
        }
    }

    private static final class ProtectedReceiptResponse extends HttpServletResponseWrapper {
        private final java.util.Map<String, String> receiptHeaders;
        private final java.util.List<String> protectedHeaders;

        private ProtectedReceiptResponse(
                HttpServletResponse response,
                java.util.Map<String, String> receiptHeaders,
                java.util.List<String> protectedHeaders) {
            super(response);
            this.receiptHeaders = java.util.Map.copyOf(receiptHeaders);
            this.protectedHeaders = java.util.List.copyOf(protectedHeaders);
            applyReceipt();
        }

        @Override public void setHeader(String name, String value) {
            if (protectedHeader(name)) {
                throw new IllegalStateException(
                        "paid handler attempted to replace settlement evidence");
            }
            super.setHeader(name, value);
        }

        @Override public void addHeader(String name, String value) {
            if (protectedHeader(name)) {
                throw new IllegalStateException(
                        "paid handler attempted to append settlement evidence");
            }
            super.addHeader(name, value);
        }

        @Override public void reset() {
            super.reset();
            applyReceipt();
        }

        private boolean protectedHeader(String name) {
            return protectedHeaders.stream().anyMatch(name::equalsIgnoreCase);
        }

        private void applyReceipt() {
            HttpServletResponse delegate = (HttpServletResponse) getResponse();
            receiptHeaders.forEach(delegate::setHeader);
        }
    }

    private static final class ReplayableRequest extends HttpServletRequestWrapper {
        private final byte[] body;

        private ReplayableRequest(HttpServletRequest request, byte[] body) {
            super(request);
            this.body = body.clone();
        }

        @Override public int getContentLength() { return body.length; }
        @Override public long getContentLengthLong() { return body.length; }
        @Override public BufferedReader getReader() {
            return new BufferedReader(new InputStreamReader(getInputStream(), StandardCharsets.UTF_8));
        }
        @Override public ServletInputStream getInputStream() {
            ByteArrayInputStream input = new ByteArrayInputStream(body);
            return new ServletInputStream() {
                @Override public boolean isFinished() { return input.available() == 0; }
                @Override public boolean isReady() { return true; }
                @Override public void setReadListener(ReadListener listener) {
                    throw new UnsupportedOperationException("async body reads are unsupported");
                }
                @Override public int read() { return input.read(); }
            };
        }
    }
}
