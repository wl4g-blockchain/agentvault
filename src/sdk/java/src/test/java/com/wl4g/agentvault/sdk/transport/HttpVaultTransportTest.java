package com.wl4g.agentvault.sdk.transport;

import static org.junit.jupiter.api.Assertions.*;

import com.sun.net.httpserver.HttpServer;
import com.wl4g.agentvault.sdk.config.Config.HttpConfig;
import java.net.InetSocketAddress;
import java.time.Duration;
import java.util.Map;
import org.junit.jupiter.api.Test;

final class HttpVaultTransportTest {

    @Test
    void sendsHttpContextAndBoundsResponses() throws Exception {
        var server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/api/v1/test", exchange -> {
            assertEquals("Bearer edge-secret", exchange.getRequestHeaders().getFirst("authorization"));
            assertTrue(exchange.getRequestHeaders().getFirst("traceparent").startsWith("00-"));
            byte[] body = "{\"ok\":true}".getBytes(java.nio.charset.StandardCharsets.UTF_8);
            exchange.sendResponseHeaders(200, body.length);
            exchange.getResponseBody().write(body);
            exchange.close();
        });
        server.start();
        try {
            String url = "http://127.0.0.1:" + server.getAddress().getPort();
            try (
                var client = new HttpVaultTransport(
                    new HttpConfig(url, "edge-secret", Duration.ofSeconds(2), 1024)
                )
            ) {
                assertEquals(Boolean.TRUE, client.getAsync("/api/v1/test", Map.class).join().get("ok"));
            }
            try (
                var client = new HttpVaultTransport(
                    new HttpConfig(url, "edge-secret", Duration.ofSeconds(2), 4)
                )
            ) {
                assertThrows(RuntimeException.class, () -> client.get("/api/v1/test", Map.class));
            }
        } finally {
            server.stop(0);
        }
    }

    @Test
    void preservesHttpErrorsAndCancelsAnExpiredBody() throws Exception {
        var server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
        server.createContext("/api/v1/denied", exchange -> {
            byte[] body = "denied by gateway".getBytes(java.nio.charset.StandardCharsets.UTF_8);
            exchange.sendResponseHeaders(401, body.length);
            exchange.getResponseBody().write(body);
            exchange.close();
        });
        server.createContext("/api/v1/slow", exchange -> {
            exchange.sendResponseHeaders(200, 0);
            try {
                Thread.sleep(500);
            } catch (InterruptedException failure) {
                Thread.currentThread().interrupt();
            } finally {
                exchange.close();
            }
        });
        server.start();
        String base = "http://127.0.0.1:" + server.getAddress().getPort();
        try {
            try (var client = new HttpVaultTransport(HttpConfig.defaults(base))) {
                var failure = assertThrows(VaultTransport.ApiException.class, () ->
                    client.get("/api/v1/denied", Map.class)
                );
                assertEquals(401, failure.status());
                assertEquals("TRANSPORT_ERROR", failure.code());
            }
            try (
                var client = new HttpVaultTransport(new HttpConfig(base, null, Duration.ofMillis(50), 1024))
            ) {
                assertTimeoutPreemptively(Duration.ofSeconds(2), () ->
                    assertThrows(RuntimeException.class, () -> client.get("/api/v1/slow", Map.class))
                );
            }
        } finally {
            server.stop(0);
        }
    }
}
