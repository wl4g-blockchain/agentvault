package io.agentvault.sdk.payer;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.sun.net.httpserver.HttpServer;

import io.agentvault.sdk.config.Config.PayerConfig;

import org.junit.jupiter.api.Test;
import org.springframework.http.client.reactive.JdkClientHttpConnector;
import org.springframework.web.reactive.function.client.WebClient;

import java.net.InetSocketAddress;
import java.nio.file.Files;
import java.nio.file.Path;
import java.time.Duration;
import java.util.concurrent.atomic.AtomicInteger;
import java.util.concurrent.atomic.AtomicReference;

final class SpringWebFluxPayerMiddlewareTest {
    private static final ObjectMapper JSON = new ObjectMapper();

    @Test
    void webClientFilterBuffersAndReplaysMcpBodiesSafely() throws Exception {
        JsonNode fixture =
                JSON.readTree(
                        Files.readString(Path.of("..", "tests", "payment_protocol_mcp.json")));
        for (String protocol : java.util.List.of("x402", "mpp")) {
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
                            JsonNode request = JSON.readTree(exchange.getRequestBody());
                            JsonNode response;
                            if (attempt == 1) {
                                response = contract.path("challenge_response");
                            } else {
                                String key =
                                        protocol.equals("mpp")
                                                ? "org.paymentauth/credential"
                                                : "x402/payment";
                                JsonNode carrier =
                                        protocol.equals("mpp")
                                                ? request.path("_meta")
                                                : request.path("params").path("_meta");
                                assertTrue(carrier.has(key));
                                assertTrue(
                                        !(protocol.equals("mpp")
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
                var webClient =
                        WebClient.builder()
                                .clientConnector(
                                        new JdkClientHttpConnector(
                                                java.net.http.HttpClient.newBuilder()
                                                        .followRedirects(
                                                                java.net.http.HttpClient.Redirect
                                                                        .NEVER)
                                                        .build()))
                                .filter(core.spring().webClientFilter())
                                .build();
                String response =
                        webClient
                                .post()
                                .uri(url)
                                .header("content-type", "application/json")
                                .header("authorization", "Bearer application-token")
                                .header("x-run-id", "run-1")
                                .bodyValue(
                                        "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\","
                                                + "\"params\":{\"name\":\"marketpulse.realtime\","
                                                + "\"arguments\":{\"symbol\":\"AVT\"}}}")
                                .retrieve()
                                .bodyToMono(String.class)
                                .block(Duration.ofSeconds(5));
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

    @Test
    void unpaidToolResponseIsNotDroppedByReactiveNullAuthorization() throws Exception {
        JsonNode fixture =
                JSON.readTree(
                        Files.readString(Path.of("..", "tests", "payment_protocol_mcp.json")));
        var server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        String expected = "{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{\"content\":[]}}";
        server.createContext(
                "/mcp",
                exchange -> {
                    exchange.getRequestBody().readAllBytes();
                    exchange.getResponseHeaders().set("content-type", "application/json");
                    exchange.getResponseHeaders().add("x-business", "first");
                    exchange.getResponseHeaders().add("x-business", "second");
                    byte[] body = expected.getBytes(java.nio.charset.StandardCharsets.UTF_8);
                    exchange.sendResponseHeaders(200, body.length);
                    exchange.getResponseBody().write(body);
                    exchange.close();
                });
        server.start();
        try {
            String url = "http://127.0.0.1:" + server.getAddress().getPort() + "/mcp";
            var transport =
                    new JdkHttpPayerMiddlewareTest.PayerTransport(
                            "x402", fixture.path("protocols").path("x402"), url);
            var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario"));
            var actual =
                    WebClient.builder()
                            .clientConnector(new JdkClientHttpConnector())
                            .filter(core.spring().webClientFilter())
                            .build()
                            .post()
                            .uri(url)
                            .header("content-type", "application/json")
                            .header("x-run-id", "free-run")
                            .bodyValue(
                                    "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"free\",\"arguments\":{}}}")
                            .retrieve()
                            .toEntity(String.class)
                            .block(Duration.ofSeconds(5));
            assertEquals(expected, actual.getBody());
            assertEquals(
                    java.util.List.of("first", "second"), actual.getHeaders().get("x-business"));
            assertTrue(transport.paths.isEmpty());
        } finally {
            server.stop(0);
        }
    }
}
