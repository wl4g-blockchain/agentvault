package io.agentvault.sdk.payee;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Base.PaymentReceipt;
import io.agentvault.sdk.model.Base.ApiException;
import io.agentvault.sdk.model.Payee.Authorized;
import io.agentvault.sdk.model.Payee.PayeeOperation;
import io.agentvault.sdk.model.Payee.PaymentRequired;
import io.agentvault.sdk.model.Payee.ProtectionOutcome;
import jakarta.servlet.FilterChain;
import jakarta.servlet.ReadListener;
import jakarta.servlet.ServletException;
import jakarta.servlet.ServletInputStream;
import jakarta.servlet.http.HttpServletRequest;
import jakarta.servlet.http.HttpServletRequestWrapper;
import jakarta.servlet.http.HttpServletResponse;
import java.io.BufferedReader;
import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.io.InputStreamReader;
import java.nio.charset.StandardCharsets;
import java.security.MessageDigest;
import java.util.Base64;
import java.util.List;
import org.springframework.web.filter.OncePerRequestFilter;

/** Spring Servlet payee adapter; register once as a Filter bean. */
public final class SpringInterceptorPayeeMiddleware extends OncePerRequestFilter {
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
            return new Route("POST", path, scenarioId, offerId, resource, origin, "x402", tool);
        }
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
        try {
            byte[] body = request.getInputStream().readNBytes(maxBodyBytes + 1);
            if (body.length > maxBodyBytes) {
                error(response, 413, "REQUEST_TOO_LARGE", "protected body exceeds limit");
                return;
            }
            var replayable = new ReplayableRequest(request, body);
            if (route.mcpTool() != null && !isMcpToolCall(body, route.mcpTool())) {
                chain.doFilter(replayable, response);
                return;
            }
            String runId = request.getHeader("x-run-id");
            String executionKey = request.getHeader("mcp-execution-key");
            if (runId == null || runId.isBlank() || executionKey == null || executionKey.isBlank()) {
                error(response, 400, "PAYMENT_CONTEXT_REQUIRED",
                        "x-run-id and mcp-execution-key are required");
                return;
            }
            JsonNode credential = decodeCredential(request.getHeader("payment-signature"));
            ProtectionOutcome outcome = core.protect(new PayeeOperation(
                    route.scenarioId(), runId, executionKey, route.offerId(), route.resource(), route.origin(),
                    route.protocol(), hash(request.getMethod(), request.getRequestURI(), body), credential));
            if (outcome instanceof PaymentRequired required) {
                PaymentChallenge challenge = required.challenge();
                JsonNode wire = challenge.payload().path("payment_required");
                if (wire.isMissingNode()) {
                    throw new IllegalStateException("AgentVault returned no protocol wire challenge");
                }
                byte[] encoded = JSON.writeValueAsBytes(wire);
                response.setStatus(402);
                response.setContentType("application/json");
                response.setHeader("payment-protocol", challenge.protocol());
                response.setHeader("payment-expires-at", Long.toString(challenge.expiresAt()));
                response.setHeader("payment-required", Base64.getEncoder().encodeToString(encoded));
                response.getOutputStream().write(encoded);
                return;
            }
            var payment = ((Authorized) outcome).payment();
            PaymentReceipt receipt = payment.receipt();
            if (receipt.transactionHash() == null || receipt.transactionHash().isBlank()
                    || receipt.payer() == null || receipt.payer().isBlank()) {
                throw new IllegalStateException("confirmed receipt omitted transaction or payer");
            }
            String evidence = Base64.getEncoder().encodeToString(JSON.writeValueAsBytes(java.util.Map.of(
                    "success", true,
                    "payer", receipt.payer(),
                    "transaction", receipt.transactionHash(),
                    "network", receipt.asset().network())));
            response.setHeader("payment-response", evidence);
            replayable.setAttribute(PAYMENT_ATTRIBUTE, payment);
            chain.doFilter(replayable, response);
        } catch (SecurityException error) {
            error(response, 402, "PAYMENT_INVALID", error.getMessage());
        } catch (ApiException error) {
            error(response, error.status(), error.code(), error.getMessage());
        } catch (IllegalArgumentException error) {
            error(response, 400, "PAYMENT_INVALID", error.getMessage());
        } catch (RuntimeException error) {
            error(response, 502, "AGENTVAULT_UNAVAILABLE", error.getMessage());
        }
    }

    private static boolean isMcpToolCall(byte[] body, String expected) {
        try {
            JsonNode request = JSON.readTree(body);
            return "tools/call".equals(request.path("method").asText())
                    && expected.equals(request.path("params").path("name").asText());
        } catch (IOException error) {
            return false;
        }
    }

    private JsonNode decodeCredential(String value) {
        if (value == null) return null;
        try {
            if (value.length() > (long) Math.ceil(maxBodyBytes * 4.0 / 3.0) + 4) {
                throw new IllegalArgumentException("PAYMENT-SIGNATURE exceeds configured limit");
            }
            byte[] body = Base64.getDecoder().decode(value);
            if (body.length > maxBodyBytes || !Base64.getEncoder().encodeToString(body).equals(value)) {
                throw new IllegalArgumentException("PAYMENT-SIGNATURE is noncanonical or oversized");
            }
            JsonNode payload = JSON.readTree(body);
            if (payload == null || !payload.isObject()) {
                throw new IllegalArgumentException("PAYMENT-SIGNATURE must be an x402 JSON object");
            }
            return payload;
        } catch (IOException | IllegalArgumentException error) {
            throw new IllegalArgumentException("invalid PAYMENT-SIGNATURE", error);
        }
    }

    private static String hash(String method, String path, byte[] body) {
        try {
            MessageDigest digest = MessageDigest.getInstance("SHA-256");
            digest.update("agentvault.payee.http.v1\0".getBytes(StandardCharsets.UTF_8));
            digest.update(method.getBytes(StandardCharsets.UTF_8));
            digest.update((byte) 0);
            digest.update(path.getBytes(StandardCharsets.UTF_8));
            digest.update((byte) 0);
            digest.update(body);
            return java.util.HexFormat.of().formatHex(digest.digest());
        } catch (java.security.NoSuchAlgorithmException error) {
            throw new IllegalStateException(error);
        }
    }

    private static void error(
            HttpServletResponse response,
            int status,
            String code,
            String message) throws IOException {
        response.reset();
        response.setStatus(status);
        response.setContentType("application/json");
        JSON.writeValue(response.getOutputStream(), java.util.Map.of(
                "error", java.util.Map.of("code", code, "message", message == null ? code : message)));
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
