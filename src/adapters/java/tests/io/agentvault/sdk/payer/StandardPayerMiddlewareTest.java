package io.agentvault.sdk.payer;

import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertEquals;
import com.sun.net.httpserver.HttpServer;
import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.model.Base.AssetIdentity;
import io.agentvault.sdk.model.Base.VaultTransport;
import java.net.InetSocketAddress;
import java.util.concurrent.atomic.AtomicInteger;
import org.junit.jupiter.api.Test;

final class StandardPayerMiddlewareTest {
    @Test void createsTheSpringEcosystemAdapterFromTheRoleEntrypoint() {
        VaultTransport transport = new NoopTransport();
        var core = new PayerMiddleware(transport, PayerConfig.defaults(
                "scenario-1", "wallet-1", "x402",
                new AssetIdentity("eip155:1", "USDC", null, 6), "100", "recipient-1"));
        assertNotNull(core.standard());
        assertNotNull(core.standard().client());
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
            var core = new PayerMiddleware(new NoopTransport(), PayerConfig.defaults(
                    "scenario-1", "wallet-1", "x402",
                    new AssetIdentity("eip155:1", "USDC", null, 6), "100", "recipient-1"));
            int status = core.standard().client().get()
                    .uri("http://127.0.0.1:" + server.getAddress().getPort() + "/start")
                    .exchange((_request, response) -> response.getStatusCode().value());
            assertEquals(302, status);
            assertEquals(0, redirects.get());
        } finally {
            server.stop(0);
        }
    }

    private static final class NoopTransport implements VaultTransport {
        @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
        @Override public <T> T post(String path, Object body, String key, Class<T> type) {
            throw new UnsupportedOperationException();
        }
    }
}
