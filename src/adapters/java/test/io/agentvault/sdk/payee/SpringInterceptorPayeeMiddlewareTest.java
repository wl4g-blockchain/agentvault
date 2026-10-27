package io.agentvault.sdk.payee;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;

import io.agentvault.sdk.config.Config.PayeeConfig;
import io.agentvault.sdk.transport.VaultTransport;

import org.junit.jupiter.api.Test;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;

import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.List;
import java.util.concurrent.atomic.AtomicBoolean;

final class SpringInterceptorPayeeMiddlewareTest {
    private static final ObjectMapper JSON = new ObjectMapper();

    @Test
    void registersOnlyExplicitlyConfiguredRoutes() {
        VaultTransport transport = new NoopTransport();
        var core = new PayeeMiddleware(transport, PayeeConfig.defaults());
        assertNotNull(
                core.spring(
                        List.of(
                                SpringInterceptorPayeeMiddleware.Route.mcpTool(
                                        "/mcp",
                                        "marketpulse.realtime_news",
                                        "scenario-1",
                                        "price-1",
                                        "mcp://marketpulse/realtime",
                                        "https://marketpulse.test"))));
    }

    @Test
    void bypassesAFreeToolOnTheSharedMcpRoute() throws Exception {
        var core = new PayeeMiddleware(new NoopTransport(), PayeeConfig.defaults());
        var filter =
                core.spring(
                        List.of(
                                SpringInterceptorPayeeMiddleware.Route.mcpTool(
                                        "/mcp",
                                        "marketpulse.realtime_news",
                                        "scenario-1",
                                        "price-1",
                                        "mcp://marketpulse/realtime",
                                        "https://marketpulse.test")));
        var request = new MockHttpServletRequest("POST", "/mcp");
        request.setContent(
                ("{\"method\":\"tools/call\",\"params\":{\"name\":"
                                + "\"marketpulse.yesterday_news\"}}")
                        .getBytes(java.nio.charset.StandardCharsets.UTF_8));
        var response = new MockHttpServletResponse();
        var passed = new AtomicBoolean();
        filter.doFilter(request, response, (_request, _response) -> passed.set(true));
        assertTrue(passed.get());
    }

    @Test
    void rejectsDuplicatedPaymentCredentialsBeforeVault() throws Exception {
        var core = new PayeeMiddleware(new NoopTransport(), PayeeConfig.defaults());
        var filter =
                core.spring(
                        List.of(
                                SpringInterceptorPayeeMiddleware.Route.mcpTool(
                                        "/mcp",
                                        "marketpulse.realtime_news",
                                        "scenario-1",
                                        "price-1",
                                        "mcp://marketpulse/realtime",
                                        "https://marketpulse.test")));
        var request = new MockHttpServletRequest("POST", "/mcp");
        request.addHeader("x-run-id", "run-1");
        request.addHeader("payment-signature", "e30=");
        request.addHeader("payment-signature", "e30=");
        request.setContent(
                ("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":"
                                + "\"marketpulse.realtime_news\"}}")
                        .getBytes(java.nio.charset.StandardCharsets.UTF_8));
        var response = new MockHttpServletResponse();
        var passed = new AtomicBoolean();

        filter.doFilter(request, response, (_request, _response) -> passed.set(true));

        assertEquals(400, response.getStatus());
        assertTrue(response.getContentAsString().contains("duplicate payment request header"));
        assertEquals(false, passed.get());
    }

    @Test
    void closesX402AndMppMcpVerifySettleFlowsAndInjectsNativeReceipts() throws Exception {
        JsonNode fixture =
                JSON.readTree(
                        Files.readString(Path.of("..", "tests", "payment_protocol_mcp.json")));
        for (String protocol : List.of("x402", "mpp")) {
            for (String placement :
                    protocol.equals("mpp") ? List.of("root", "params") : List.of("params")) {
                JsonNode contract = fixture.path("protocols").path(protocol);
                var transport = new PayeeTransport(protocol, contract, true);
                var core = new PayeeMiddleware(transport, PayeeConfig.defaults());
                var filter =
                        core.spring(
                                List.of(
                                        SpringInterceptorPayeeMiddleware.Route.mcpToolWithProtocol(
                                                "/mcp",
                                                "marketpulse.realtime",
                                                "scenario",
                                                "price-1",
                                                "mcp://tool/marketpulse.realtime",
                                                "https://merchant.test",
                                                protocol)));

                ObjectNode call = mcpCall();
                if (protocol.equals("mpp")) {
                    call.putObject("_meta").put("application/root", "preserved");
                }
                var firstRequest = request(call);
                var firstResponse = new MockHttpServletResponse();
                var firstPassed = new AtomicBoolean();
                filter.doFilter(
                        firstRequest,
                        firstResponse,
                        (_request, _response) -> firstPassed.set(true));
                assertEquals(200, firstResponse.getStatus());
                assertEquals(false, firstPassed.get());
                assertEquals(protocol, firstResponse.getHeader("payment-protocol"));
                assertEquals("2000000000", firstResponse.getHeader("payment-expires-at"));
                JsonNode challenge = JSON.readTree(firstResponse.getContentAsByteArray());
                if (protocol.equals("x402")) {
                    assertTrue(challenge.path("result").path("isError").asBoolean());
                    assertEquals(
                            2,
                            challenge
                                    .path("result")
                                    .path("structuredContent")
                                    .path("x402Version")
                                    .asInt());
                } else {
                    assertEquals(-32042, challenge.path("error").path("code").asInt());
                }

                ObjectNode paidCall = call.deepCopy();
                if (protocol.equals("mpp") && placement.equals("root")) {
                    ((ObjectNode) paidCall.path("_meta"))
                            .setAll((ObjectNode) contract.path("credential_meta"));
                } else {
                    ((ObjectNode) paidCall.path("params"))
                            .set("_meta", contract.path("credential_meta"));
                }
                var paidRequest = request(paidCall);
                var paidResponse = new MockHttpServletResponse();
                var paidPassed = new AtomicBoolean();
                filter.doFilter(
                        paidRequest,
                        paidResponse,
                        (_request, servletResponse) -> {
                            paidPassed.set(true);
                            JsonNode forwarded = JSON.readTree(_request.getInputStream());
                            JsonNode forwardedMeta = forwarded.path("params").path("_meta");
                            assertFalse(forwardedMeta.has("x402/payment"));
                            assertFalse(forwardedMeta.has("org.paymentauth/credential"));
                            JsonNode forwardedRootMeta = forwarded.path("_meta");
                            assertFalse(forwardedRootMeta.has("org.paymentauth/credential"));
                            if (protocol.equals("mpp")) {
                                assertEquals(
                                        "preserved",
                                        forwardedRootMeta.path("application/root").asText());
                            }
                            servletResponse.setContentType("application/json");
                            ObjectNode response = JSON.createObjectNode();
                            response.put("jsonrpc", "2.0");
                            response.put("id", 1);
                            response.putObject("result")
                                    .putArray("content")
                                    .addObject()
                                    .put("type", "text")
                                    .put("text", "paid");
                            JSON.writeValue(servletResponse.getOutputStream(), response);
                        });

                assertEquals(200, paidResponse.getStatus());
                assertTrue(paidPassed.get());
                JsonNode paid = JSON.readTree(paidResponse.getContentAsByteArray());
                String receiptKey =
                        protocol.equals("x402")
                                ? "x402/payment-response"
                                : "org.paymentauth/receipt";
                assertTrue(paid.path("result").path("_meta").has(receiptKey));
                assertNull(paidResponse.getHeader("payment-response"));
                assertNull(paidResponse.getHeader("payment-receipt"));
                assertEquals(
                        List.of(
                                "/api/v1/payee/challenges",
                                "/api/v1/payee/challenges",
                                "/api/v1/payee/verify",
                                "/api/v1/payee/settle"),
                        transport.paths);
                assertEquals(transport.requestHashes.get(0), transport.requestHashes.get(1));
                assertEquals(protocol, transport.presentedCredentialProtocol);
                if (protocol.equals("mpp")) {
                    ObjectNode ambiguousCall = paidCall.deepCopy();
                    ((ObjectNode) ambiguousCall.path("params"))
                            .set("_meta", contract.path("credential_meta"));
                    ((ObjectNode) ambiguousCall.path("_meta"))
                            .setAll((ObjectNode) contract.path("credential_meta"));
                    var ambiguousResponse = new MockHttpServletResponse();
                    var ambiguousPassed = new AtomicBoolean();
                    filter.doFilter(
                            request(ambiguousCall),
                            ambiguousResponse,
                            (_request, _response) -> ambiguousPassed.set(true));
                    assertEquals(400, ambiguousResponse.getStatus());
                    assertTrue(
                            ambiguousResponse
                                    .getContentAsString()
                                    .contains("duplicate MCP payment credential metadata"));
                    assertFalse(ambiguousPassed.get());
                    assertEquals(4, transport.paths.size());
                }
            }
        }
    }

    @Test
    void mapsRejectedMppMcpCredentialsToVerificationError() throws Exception {
        JsonNode fixture =
                JSON.readTree(
                        Files.readString(Path.of("..", "tests", "payment_protocol_mcp.json")));
        JsonNode contract = fixture.path("protocols").path("mpp");
        var transport = new PayeeTransport("mpp", contract, false);
        var filter =
                new PayeeMiddleware(transport, PayeeConfig.defaults())
                        .spring(
                                List.of(
                                        SpringInterceptorPayeeMiddleware.Route.mcpToolWithProtocol(
                                                "/mcp",
                                                "marketpulse.realtime",
                                                "scenario",
                                                "price-1",
                                                "mcp://tool/marketpulse.realtime",
                                                "https://merchant.test",
                                                "mpp")));
        ObjectNode paidCall = mcpCall();
        ((ObjectNode) paidCall.path("params")).set("_meta", contract.path("credential_meta"));
        var response = new MockHttpServletResponse();
        filter.doFilter(
                request(paidCall),
                response,
                (_request, _response) -> {
                    throw new AssertionError("business handler must not run");
                });
        JsonNode message = JSON.readTree(response.getContentAsByteArray());
        assertEquals(200, response.getStatus());
        assertEquals(-32043, message.path("error").path("code").asInt());
        assertEquals(402, message.path("error").path("data").path("httpStatus").asInt());
        assertEquals(
                "challenge-0123456789abcdef",
                message.path("error").path("data").path("problem").path("challengeId").asText());
    }

    @Test
    void preservesPaidMcpBusinessFailureForDeliveryRetry() throws Exception {
        JsonNode fixture =
                JSON.readTree(
                        Files.readString(Path.of("..", "tests", "payment_protocol_mcp.json")));
        JsonNode contract = fixture.path("protocols").path("x402");
        var filter =
                new PayeeMiddleware(
                                new PayeeTransport("x402", contract, true), PayeeConfig.defaults())
                        .spring(
                                List.of(
                                        SpringInterceptorPayeeMiddleware.Route.mcpTool(
                                                "/mcp",
                                                "marketpulse.realtime",
                                                "scenario",
                                                "price-1",
                                                "mcp://tool/marketpulse.realtime",
                                                "https://merchant.test")));
        ObjectNode paidCall = mcpCall();
        ((ObjectNode) paidCall.path("params")).set("_meta", contract.path("credential_meta"));
        var response = new MockHttpServletResponse();
        filter.doFilter(
                request(paidCall),
                response,
                (_request, servletResponse) -> {
                    var httpResponse = (jakarta.servlet.http.HttpServletResponse) servletResponse;
                    httpResponse.setStatus(503);
                    httpResponse.setContentType("application/json");
                    ObjectNode message = JSON.createObjectNode();
                    message.put("jsonrpc", "2.0");
                    message.put("id", 1);
                    message.putObject("error").put("code", -32004).put("message", "retry delivery");
                    JSON.writeValue(httpResponse.getOutputStream(), message);
                });
        JsonNode message = JSON.readTree(response.getContentAsByteArray());
        assertEquals(503, response.getStatus());
        assertEquals("retry delivery", message.path("error").path("message").asText());
        assertFalse(message.has("result"));
    }

    private static ObjectNode mcpCall() {
        ObjectNode call = JSON.createObjectNode();
        call.put("jsonrpc", "2.0");
        call.put("id", 1);
        call.put("method", "tools/call");
        ObjectNode params = call.putObject("params");
        params.put("name", "marketpulse.realtime");
        params.putObject("arguments").put("symbol", "AVT");
        return call;
    }

    private static MockHttpServletRequest request(JsonNode body) throws java.io.IOException {
        var request = new MockHttpServletRequest("POST", "/mcp");
        request.addHeader("x-run-id", "run-1");
        request.addHeader("authorization", "Bearer application-token");
        request.setContentType("application/json");
        request.setContent(JSON.writeValueAsBytes(body));
        return request;
    }

    private static final class PayeeTransport implements VaultTransport {
        private final String protocol;
        private final JsonNode contract;
        private final boolean valid;
        private final List<String> paths = new ArrayList<>();
        private final List<String> requestHashes = new ArrayList<>();
        private String presentedCredentialProtocol;

        private PayeeTransport(String protocol, JsonNode contract, boolean valid) {
            this.protocol = protocol;
            this.contract = contract;
            this.valid = valid;
        }

        @Override
        public <T> T get(String path, Class<T> type) {
            throw new UnsupportedOperationException();
        }

        @Override
        public <T> T post(String path, Object body, Class<T> type) {
            paths.add(path);
            JsonNode request = JSON.valueToTree(body);
            try {
                if (path.equals("/api/v1/payee/challenges")) {
                    requestHashes.add(request.path("request_hash").asText());
                    ObjectNode challenge = JSON.createObjectNode();
                    challenge.put("scenario_id", "scenario");
                    challenge.put("price_id", "price-1");
                    challenge.put("resource", "mcp://tool/marketpulse.realtime");
                    challenge.put("protocol", protocol);
                    challenge
                            .putObject("asset")
                            .put("network", "eip155:42431")
                            .put("asset", "USDC")
                            .put("decimals", 6);
                    challenge.put("amount", "1000");
                    challenge.put("recipient", "0x1111111111111111111111111111111111111111");
                    challenge.put(
                            "nonce",
                            protocol.equals("mpp") ? "challenge-0123456789abcdef" : "nonce-1");
                    challenge.put("challenge_hash", "challenge-1");
                    challenge.put("expires_at", 2_000_000_000L);
                    ObjectNode payload = challenge.putObject("payload");
                    if (protocol.equals("x402")) {
                        payload.set(
                                "payment_required",
                                contract.path("challenge_response")
                                        .path("result")
                                        .path("structuredContent"));
                    } else {
                        payload.set(
                                "mcp_challenge",
                                contract.path("challenge_response")
                                        .path("error")
                                        .path("data")
                                        .path("challenges")
                                        .get(0));
                    }
                    return JSON.treeToValue(challenge, type);
                }
                if (path.equals("/api/v1/payee/verify")) {
                    presentedCredentialProtocol =
                            request.path("credential").path("protocol").asText();
                    ObjectNode response = JSON.createObjectNode();
                    response.set("intent", intent(false));
                    response.put("valid", valid);
                    if (valid) response.put("verification_ref", "verification-1");
                    return JSON.treeToValue(response, type);
                }
                if (path.equals("/api/v1/payee/settle")) {
                    ObjectNode response = JSON.createObjectNode();
                    response.set("intent", intent(true));
                    ObjectNode receipt = response.putObject("receipt");
                    receipt.put("intent_id", "intent-1");
                    receipt.put("scenario_id", "scenario");
                    receipt.put("direction", "inbound");
                    receipt.put("protocol", protocol);
                    receipt.putObject("asset")
                            .put("network", "eip155:42431")
                            .put("asset", "USDC")
                            .put("decimals", 6);
                    receipt.put("recipient", "0x1111111111111111111111111111111111111111");
                    receipt.put("payer", "0x3333333333333333333333333333333333333333");
                    receipt.put("requested_amount", "1000");
                    receipt.put("settled_amount", "1000");
                    receipt.put("status", "settled");
                    receipt.put("tx_hash", "0x1111");
                    receipt.put("evidence_hash", "evidence-1");
                    receipt.put("confirmed_at", 2_000_000_000L);
                    return JSON.treeToValue(response, type);
                }
                throw new AssertionError("unexpected AgentVault path: " + path);
            } catch (com.fasterxml.jackson.core.JsonProcessingException error) {
                throw new IllegalStateException(error);
            }
        }

        private ObjectNode intent(boolean settled) {
            ObjectNode value = JSON.createObjectNode();
            value.put("id", "intent-1");
            value.put("scenario_id", "scenario");
            value.put("run_id", "run-1");
            value.put("wallet_id", "wallet-1");
            value.put("policy_id", "policy-1");
            value.put("price_id", "price-1");
            value.put("direction", "inbound");
            value.put("protocol", protocol);
            value.putObject("asset")
                    .put("network", "eip155:42431")
                    .put("asset", "USDC")
                    .put("decimals", 6);
            value.put("recipient", "0x1111111111111111111111111111111111111111");
            value.put("amount", "1000");
            value.put("settled_amount", settled ? "1000" : "0");
            value.put("resource", "mcp://tool/marketpulse.realtime");
            value.put("challenge_hash", "challenge-1");
            value.put("request_hash", requestHashes.get(requestHashes.size() - 1));
            value.put("status", settled ? "settled" : "created");
            value.put("reservation_status", settled ? "consumed" : "unreserved");
            value.put("reserved_amount", "0");
            value.put("tx_hash", settled ? "0x1111" : null);
            value.put("created_at", 1);
            value.put("updated_at", 2);
            if (settled) value.put("settled_at", 2_000_000_000L);
            return value;
        }
    }

    private static final class NoopTransport implements VaultTransport {
        @Override
        public <T> T get(String path, Class<T> type) {
            throw new UnsupportedOperationException();
        }

        @Override
        public <T> T post(String path, Object body, Class<T> type) {
            throw new UnsupportedOperationException();
        }
    }
}
