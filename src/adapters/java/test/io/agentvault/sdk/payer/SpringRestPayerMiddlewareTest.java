package io.agentvault.sdk.payer;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.sun.net.httpserver.HttpServer;

import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.transport.VaultTransport;

import org.junit.jupiter.api.Test;

import java.net.InetSocketAddress;
import java.nio.file.Files;
import java.nio.file.Path;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

final class SpringRestPayerMiddlewareTest {
    private static final ObjectMapper JSON = new ObjectMapper();

    @Test
    void createsTheSpringEcosystemAdapterFromTheRoleEntrypoint() {
        VaultTransport transport = new NoopTransport();
        var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario-1"));
        assertNotNull(core.spring());
        assertNotNull(core.spring().client());
    }

    @Test
    void spring7A2aHeaderAdaptationPreservesRequestAttributesAndRepeatedHeaders() throws Exception {
        var request =
                new org.springframework.mock.http.client.MockClientHttpRequest(
                        org.springframework.http.HttpMethod.POST,
                        java.net.URI.create("http://127.0.0.1/a2a"));
        request.getHeaders().set("x-run-id", "attributes-run");
        request.getHeaders().setContentType(org.springframework.http.MediaType.APPLICATION_JSON);
        request.getHeaders().add("x-business", "first");
        request.getHeaders().add("x-business", "second");
        request.getAttributes().put("business-context", "retained");
        var core = new PayerMiddleware(new NoopTransport(), PayerConfig.defaults("scenario-1"));
        byte[] body =
                ("{\"jsonrpc\":\"2.0\",\"id\":\"attrs\",\"method\":\"message/send\","
                     + "\"params\":{\"message\":{\"kind\":\"message\",\"messageId\":\"attributes-message\","
                     + "\"role\":\"user\",\"parts\":[{\"kind\":\"text\",\"text\":\"ping\"}]}}}")
                        .getBytes(java.nio.charset.StandardCharsets.UTF_8);
        try (var response =
                core.spring()
                        .intercept(
                                request,
                                body,
                                (adapted, bytes) -> {
                                    assertSame(request.getAttributes(), adapted.getAttributes());
                                    assertEquals(
                                            "retained",
                                            adapted.getAttributes().get("business-context"));
                                    assertEquals(
                                            List.of("first", "second"),
                                            adapted.getHeaders().get("x-business"));
                                    assertNotNull(
                                            adapted.getHeaders().getFirst("x-a2a-extensions"));
                                    return new org.springframework.mock.http.client
                                            .MockClientHttpResponse(
                                            new byte[0],
                                            org.springframework.http.HttpStatus.NO_CONTENT);
                                })) {
            assertEquals(204, response.getStatusCode().value());
        }
        assertNull(request.getHeaders().getFirst("x-a2a-extensions"));
    }

    @Test
    void safeClientNeverFollowsRedirects() throws Exception {
        var redirects = new AtomicInteger();
        var server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext(
                "/start",
                exchange -> {
                    exchange.getResponseHeaders().set("location", "/credential-leak");
                    exchange.sendResponseHeaders(302, -1);
                    exchange.close();
                });
        server.createContext(
                "/credential-leak",
                exchange -> {
                    redirects.incrementAndGet();
                    exchange.sendResponseHeaders(204, -1);
                    exchange.close();
                });
        server.start();
        try {
            var core = new PayerMiddleware(new NoopTransport(), PayerConfig.defaults("scenario-1"));
            int status =
                    core.spring()
                            .client()
                            .get()
                            .uri("http://127.0.0.1:" + server.getAddress().getPort() + "/start")
                            .exchange((_request, response) -> response.getStatusCode().value());
            assertEquals(302, status);
            assertEquals(0, redirects.get());
        } finally {
            server.stop(0);
        }
    }

    @Test
    void springClientClosesBothMcpProtocolFlowsWithoutPaymentHeaders() throws Exception {
        JsonNode fixture =
                JSON.readTree(
                        Files.readString(Path.of("..", "tests", "payment_protocol_mcp.json")));
        for (String protocol : List.of("x402", "mpp")) {
            JsonNode contract = fixture.path("protocols").path(protocol);
            var attempts = new AtomicInteger();
            var failure = new AtomicReference<Throwable>();
            var server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
            server.createContext(
                    "/mcp",
                    exchange -> {
                        try {
                            int attempt = attempts.incrementAndGet();
                            assertEquals(
                                    "Bearer application-token",
                                    exchange.getRequestHeaders().getFirst("authorization"));
                            assertNull(exchange.getRequestHeaders().getFirst("payment-signature"));
                            assertNull(
                                    exchange.getRequestHeaders().getFirst("payment-authorization"));
                            JsonNode request = JSON.readTree(exchange.getRequestBody());
                            JsonNode response;
                            if (attempt == 1) {
                                response = contract.path("challenge_response");
                            } else {
                                String key =
                                        protocol.equals("x402")
                                                ? "x402/payment"
                                                : "org.paymentauth/credential";
                                JsonNode carrier =
                                        protocol.equals("mpp")
                                                ? request.path("_meta")
                                                : request.path("params").path("_meta");
                                assertEquals(key, carrier.fieldNames().next());
                                assertEquals(1, carrier.size());
                                assertFalse(
                                        (protocol.equals("mpp")
                                                        ? request.path("params").path("_meta")
                                                        : request.path("_meta"))
                                                .has(key));
                                ObjectNode paid = JSON.createObjectNode();
                                paid.put("jsonrpc", "2.0");
                                paid.set("id", request.get("id"));
                                ObjectNode result = paid.putObject("result");
                                result.putArray("content")
                                        .addObject()
                                        .put("type", "text")
                                        .put("text", "paid");
                                result.set("_meta", contract.path("receipt_meta"));
                                response = paid;
                            }
                            byte[] encoded = JSON.writeValueAsBytes(response);
                            exchange.getResponseHeaders().set("content-type", "application/json");
                            exchange.sendResponseHeaders(200, encoded.length);
                            exchange.getResponseBody().write(encoded);
                            exchange.close();
                        } catch (Throwable error) {
                            failure.compareAndSet(null, error);
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
                var transport =
                        new JdkHttpPayerMiddlewareTest.PayerTransport(protocol, contract, url);
                var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario"));
                String response =
                        core.spring()
                                .client()
                                .post()
                                .uri(url)
                                .header("content-type", "application/json")
                                .header("authorization", "Bearer application-token")
                                .header("x-run-id", "run-1")
                                .body(
                                        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\","
                                                + "\"params\":{\"name\":\"marketpulse.realtime\","
                                                + "\"arguments\":{\"symbol\":\"AVT\"}}}")
                                .retrieve()
                                .body(String.class);
                assertEquals(
                        "paid",
                        JSON.readTree(response)
                                .path("result")
                                .path("content")
                                .get(0)
                                .path("text")
                                .asText());
                assertEquals(2, attempts.get());
                assertEquals(2, transport.paths.size());
                assertEquals(protocol, transport.report.path("protocol").asText());
                assertNull(failure.get());
            } finally {
                server.stop(0);
            }
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
