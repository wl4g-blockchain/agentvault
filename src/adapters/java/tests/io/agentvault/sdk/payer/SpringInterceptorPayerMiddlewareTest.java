package io.agentvault.sdk.payer;

import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.sun.net.httpserver.HttpServer;
import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.transport.VaultTransport;
import java.net.InetSocketAddress;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.List;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;
import org.springframework.http.client.reactive.JdkClientHttpConnector;
import org.springframework.web.reactive.function.client.WebClient;
import org.junit.jupiter.api.Test;

final class SpringInterceptorPayerMiddlewareTest {
    private static final ObjectMapper JSON = new ObjectMapper();
    @Test void createsTheSpringEcosystemAdapterFromTheRoleEntrypoint() {
        VaultTransport transport = new NoopTransport();
        var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario-1"));
        assertNotNull(core.spring());
        assertNotNull(core.spring().client());
    }

    @Test void safeClientNeverFollowsRedirects() throws Exception {
        var redirects = new AtomicInteger();
        var server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/start", exchange -> {
            exchange.getResponseHeaders().set("location", "/credential-leak");
            exchange.sendResponseHeaders(302, -1);
            exchange.close();
        });
        server.createContext("/credential-leak", exchange -> {
            redirects.incrementAndGet();
            exchange.sendResponseHeaders(204, -1);
            exchange.close();
        });
        server.start();
        try {
            var core = new PayerMiddleware(new NoopTransport(), PayerConfig.defaults("scenario-1"));
            int status = core.spring().client().get()
                    .uri("http://127.0.0.1:" + server.getAddress().getPort() + "/start")
                    .exchange((_request, response) -> response.getStatusCode().value());
            assertEquals(302, status);
            assertEquals(0, redirects.get());
        } finally {
            server.stop(0);
        }
    }

    @Test void springClientClosesBothMcpProtocolFlowsWithoutPaymentHeaders() throws Exception {
        JsonNode fixture = JSON.readTree(Files.readString(
                Path.of("..", "fixtures", "payment_protocol_mcp.json")));
        for (String protocol : List.of("x402", "mpp")) {
            JsonNode contract = fixture.path("protocols").path(protocol);
            var attempts = new AtomicInteger();
            var failure = new AtomicReference<Throwable>();
            var server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
            server.createContext("/mcp", exchange -> {
                try {
                    int attempt = attempts.incrementAndGet();
                    assertEquals("Bearer application-token",
                            exchange.getRequestHeaders().getFirst("authorization"));
                    assertNull(exchange.getRequestHeaders().getFirst("payment-signature"));
                    assertNull(exchange.getRequestHeaders().getFirst("payment-authorization"));
                    JsonNode request = JSON.readTree(exchange.getRequestBody());
                    JsonNode response;
                    if (attempt == 1) {
                        response = contract.path("challenge_response");
                    } else {
                        String key = protocol.equals("x402")
                                ? "x402/payment" : "org.paymentauth/credential";
                        assertEquals(List.of(key), java.util.stream.StreamSupport.stream(
                                java.util.Spliterators.spliteratorUnknownSize(
                                        request.path("params").path("_meta").fieldNames(), 0), false)
                                .toList());
                        ObjectNode paid = JSON.createObjectNode();
                        paid.put("jsonrpc", "2.0");
                        paid.set("id", request.get("id"));
                        ObjectNode result = paid.putObject("result");
                        result.putArray("content").addObject()
                                .put("type", "text").put("text", "paid");
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
                var transport = new JdkHttpPayerMiddlewareTest.PayerTransport(
                        protocol, contract, url);
                var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario"));
                String response = core.spring().client().post().uri(url)
                        .header("content-type", "application/json")
                        .header("authorization", "Bearer application-token")
                        .header("x-run-id", "run-1")
                        .body("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\","
                                + "\"params\":{\"name\":\"marketpulse.realtime\","
                                + "\"arguments\":{\"symbol\":\"AVT\"}}}")
                        .retrieve().body(String.class);
                assertEquals("paid", JSON.readTree(response)
                        .path("result").path("content").get(0).path("text").asText());
                assertEquals(2, attempts.get());
                assertEquals(2, transport.paths.size());
                assertEquals(protocol, transport.report.path("protocol").asText());
                assertNull(failure.get());
            } finally {
                server.stop(0);
            }
        }
    }

    @Test void webClientFilterBuffersAndReplaysMcpBodiesSafely() throws Exception {
        JsonNode fixture = JSON.readTree(Files.readString(
                Path.of("..", "fixtures", "payment_protocol_mcp.json")));
        String protocol = "x402";
        JsonNode contract = fixture.path("protocols").path(protocol);
        var attempts = new AtomicInteger();
        var failure = new AtomicReference<Throwable>();
        var server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/mcp", exchange -> {
            try {
                int attempt = attempts.incrementAndGet();
                assertEquals("Bearer application-token",
                        exchange.getRequestHeaders().getFirst("authorization"));
                assertNull(exchange.getRequestHeaders().getFirst("payment-signature"));
                JsonNode request = JSON.readTree(exchange.getRequestBody());
                JsonNode response;
                if (attempt == 1) {
                    response = contract.path("challenge_response");
                } else {
                    assertTrue(request.path("params").path("_meta").has("x402/payment"));
                    ObjectNode paid = JSON.createObjectNode();
                    paid.put("jsonrpc", "2.0");
                    paid.set("id", request.get("id"));
                    ObjectNode result = paid.putObject("result");
                    result.putArray("content").addObject()
                            .put("type", "text").put("text", "paid");
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
            var transport = new JdkHttpPayerMiddlewareTest.PayerTransport(protocol, contract, url);
            var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario"));
            var webClient = WebClient.builder()
                    .clientConnector(new JdkClientHttpConnector(
                            java.net.http.HttpClient.newBuilder()
                                    .followRedirects(java.net.http.HttpClient.Redirect.NEVER).build()))
                    .filter(core.spring().webClientFilter())
                    .build();
            String response = webClient.post().uri(url)
                    .header("content-type", "application/json")
                    .header("authorization", "Bearer application-token")
                    .header("x-run-id", "run-1")
                    .bodyValue("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\","
                            + "\"params\":{\"name\":\"marketpulse.realtime\","
                            + "\"arguments\":{\"symbol\":\"AVT\"}}}")
                    .retrieve().bodyToMono(String.class).block(Duration.ofSeconds(5));
            assertEquals("paid", JSON.readTree(response)
                    .path("result").path("content").get(0).path("text").asText());
            assertEquals(2, attempts.get());
            assertEquals(2, transport.paths.size());
            assertNull(failure.get());
        } finally {
            server.stop(0);
        }
    }

    private static final class NoopTransport implements VaultTransport {
        @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
        @Override public <T> T post(String path, Object body, Class<T> type) {
            throw new UnsupportedOperationException();
        }
    }
}
