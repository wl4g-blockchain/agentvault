package io.agentvault.sdk.payer;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.sun.net.httpserver.HttpServer;

import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.model.Payer.PayerRequestContext;
import io.agentvault.sdk.protocol.MppAdapter;
import io.agentvault.sdk.transport.VaultTransport;

import org.junit.jupiter.api.Test;

import java.net.InetSocketAddress;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.ArrayList;
import java.util.Base64;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

final class JdkHttpPayerMiddlewareTest {
    private static final ObjectMapper JSON = new ObjectMapper();

    @Test
    void rejectsClientsThatCanForwardCredentialsAcrossRedirects() {
        VaultTransport transport =
                new VaultTransport() {
                    @Override
                    public <T> T get(String path, Class<T> type) {
                        throw new UnsupportedOperationException();
                    }

                    @Override
                    public <T> T post(String path, Object body, Class<T> type) {
                        throw new UnsupportedOperationException();
                    }
                };
        var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario"));
        var redirecting =
                HttpClient.newBuilder().followRedirects(HttpClient.Redirect.NORMAL).build();
        assertThrows(
                IllegalArgumentException.class,
                () -> new JdkHttpPayerMiddleware(core, redirecting));
    }

    @Test
    void closesX402AndMppMcpMetadataFlowsExactlyOnce() throws Exception {
        JsonNode fixture =
                JSON.readTree(
                        Files.readString(Path.of("..", "tests", "payment_protocol_mcp.json")));
        for (String protocol : List.of("x402", "mpp")) {
            JsonNode contract = fixture.path("protocols").path(protocol);
            var requests = new AtomicInteger();
            var handlerFailure = new AtomicReference<Throwable>();
            HttpServer server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
            server.createContext(
                    "/mcp",
                    exchange -> {
                        try {
                            int attempt = requests.incrementAndGet();
                            assertEquals(
                                    "Bearer application-token",
                                    exchange.getRequestHeaders().getFirst("authorization"));
                            assertNull(exchange.getRequestHeaders().getFirst("payment-signature"));
                            assertNull(
                                    exchange.getRequestHeaders().getFirst("payment-authorization"));
                            JsonNode request = JSON.readTree(exchange.getRequestBody());
                            if (attempt == 1) {
                                assertFalse(request.path("params").has("_meta"));
                                respond(exchange, contract.path("challenge_response"));
                            } else {
                                String key =
                                        protocol.equals("x402")
                                                ? "x402/payment"
                                                : "org.paymentauth/credential";
                                assertEquals(
                                        SetLike.of(key),
                                        SetLike.from(
                                                protocol.equals("mpp")
                                                        ? request.path("_meta")
                                                        : request.path("params").path("_meta")));
                                assertFalse(
                                        (protocol.equals("mpp")
                                                        ? request.path("params").path("_meta")
                                                        : request.path("_meta"))
                                                .has(key));
                                ObjectNode response = JSON.createObjectNode();
                                response.put("jsonrpc", "2.0");
                                response.set("id", request.get("id"));
                                ObjectNode result = response.putObject("result");
                                result.putArray("content")
                                        .addObject()
                                        .put("type", "text")
                                        .put("text", "paid");
                                result.set("_meta", contract.path("receipt_meta"));
                                respond(exchange, response);
                            }
                        } catch (Throwable error) {
                            handlerFailure.compareAndSet(null, error);
                            try {
                                exchange.sendResponseHeaders(500, -1);
                            } finally {
                                exchange.close();
                            }
                        }
                    });
            server.start();
            try {
                String url = "http://127.0.0.1:" + server.getAddress().getPort() + "/mcp";
                var transport = new PayerTransport(protocol, contract, url);
                var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario"));
                var middleware =
                        new JdkHttpPayerMiddleware(
                                core,
                                HttpClient.newBuilder()
                                        .followRedirects(HttpClient.Redirect.NEVER)
                                        .build());
                byte[] body =
                        ("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\","
                                        + "\"params\":{\"name\":\"marketpulse.realtime\","
                                        + "\"arguments\":{\"symbol\":\"AVT\"}}}")
                                .getBytes(java.nio.charset.StandardCharsets.UTF_8);
                HttpRequest request =
                        HttpRequest.newBuilder(URI.create(url))
                                .header("content-type", "application/json")
                                .header("authorization", "Bearer application-token")
                                .POST(HttpRequest.BodyPublishers.ofByteArray(body))
                                .build();
                var result = middleware.send(request, body, new PayerRequestContext("run-1"));
                assertEquals(200, result.response().statusCode());
                assertEquals(
                        "paid",
                        JSON.readTree(result.response().body())
                                .path("result")
                                .path("content")
                                .get(0)
                                .path("text")
                                .asText());
                assertEquals("intent-1", result.payment().intent().id());
                assertEquals(2, requests.get());
                assertEquals(
                        List.of(
                                "/api/v1/payments/authorize",
                                "/api/v1/payments/intent-1/outbound-response"),
                        transport.paths);
                assertEquals(protocol, transport.report.path("protocol").asText());
                assertNull(handlerFailure.get());
            } finally {
                server.stop(0);
            }
        }
    }

    private static void respond(com.sun.net.httpserver.HttpExchange exchange, JsonNode body)
            throws java.io.IOException {
        byte[] encoded = JSON.writeValueAsBytes(body);
        exchange.getResponseHeaders().set("content-type", "application/json");
        exchange.sendResponseHeaders(200, encoded.length);
        exchange.getResponseBody().write(encoded);
        exchange.close();
    }

    static final class PayerTransport implements VaultTransport {
        private final String protocol;
        private final JsonNode contract;
        private final String resource;
        final List<String> paths = new ArrayList<>();
        JsonNode report;

        PayerTransport(String protocol, JsonNode contract, String resource) {
            this.protocol = protocol;
            this.contract = contract;
            this.resource = resource;
        }

        @Override
        public <T> T get(String path, Class<T> type) {
            throw new UnsupportedOperationException();
        }

        @Override
        public <T> T post(String path, Object body, Class<T> type) {
            paths.add(path);
            try {
                if (path.equals("/api/v1/payments/authorize")) {
                    ObjectNode response = JSON.createObjectNode();
                    response.putObject("authorization")
                            .put("outcome", "authorized")
                            .put("intent_id", "intent-1");
                    ObjectNode intent = intent();
                    response.set("intent", intent);
                    JsonNode meta = contract.path("credential_meta");
                    JsonNode value =
                            protocol.equals("x402")
                                    ? meta.path("x402/payment")
                                    : meta.path("org.paymentauth/credential");
                    String headerValue =
                            protocol.equals("x402")
                                    ? Base64.getEncoder()
                                            .encodeToString(JSON.writeValueAsBytes(value))
                                    : new MppAdapter()
                                            .presentedMcpCredential(meta, 64 * 1024)
                                            .path("authorization")
                                            .asText();
                    ObjectNode credential = response.putObject("credential");
                    credential.put("protocol", protocol);
                    credential.put("challenge_hash", "challenge-1");
                    credential.put("credential_hash", "credential-1");
                    credential.set("payload", value);
                    credential
                            .putObject("http")
                            .put(
                                    "bound_origin",
                                    URI.create(resource).getScheme()
                                            + "://"
                                            + URI.create(resource).getAuthority())
                            .put(
                                    "header_name",
                                    protocol.equals("x402")
                                            ? "Payment-Signature"
                                            : "Payment-Authorization")
                            .put("header_value", headerValue);
                    return JSON.treeToValue(response, type);
                }
                if (path.equals("/api/v1/payments/intent-1/outbound-response")) {
                    report = JSON.valueToTree(body);
                    ObjectNode response = JSON.createObjectNode();
                    response.set("intent", intent());
                    response.put("accepted", true);
                    response.put("replayed", false);
                    return JSON.treeToValue(response, type);
                }
                throw new AssertionError("unexpected AgentVault path: " + path);
            } catch (com.fasterxml.jackson.core.JsonProcessingException error) {
                throw new IllegalStateException(error);
            }
        }

        private ObjectNode intent() {
            ObjectNode value = JSON.createObjectNode();
            value.put("id", "intent-1");
            value.put("scenario_id", "scenario");
            value.put("run_id", "run-1");
            value.put("wallet_id", "wallet-1");
            value.put("policy_id", "policy-1");
            value.put("direction", "outbound");
            value.put("protocol", protocol);
            value.putObject("asset")
                    .put("network", "eip155:42431")
                    .put("asset", "USDC")
                    .put("decimals", 6);
            value.put("recipient", "0x1111111111111111111111111111111111111111");
            value.put("amount", "1000");
            value.put("settled_amount", "0");
            value.put("resource", resource);
            value.put("challenge_hash", "challenge-1");
            value.put("request_hash", "request-1");
            value.put("status", "signed");
            value.put("reservation_status", "reserved");
            value.put("reserved_amount", "1000");
            value.put("created_at", 1);
            value.put("updated_at", 2);
            return value;
        }
    }

    private record SetLike(java.util.Set<String> values) {
        private static SetLike of(String value) {
            return new SetLike(java.util.Set.of(value));
        }

        private static SetLike from(JsonNode object) {
            var values = new java.util.HashSet<String>();
            object.fieldNames().forEachRemaining(values::add);
            return new SetLike(values);
        }
    }
}
