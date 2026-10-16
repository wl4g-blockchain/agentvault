package io.agentvault.sdk.payee;

import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;
import io.agentvault.sdk.config.Config.PayeeConfig;
import io.agentvault.sdk.model.Base.VaultTransport;
import java.util.List;
import java.util.concurrent.atomic.AtomicBoolean;
import org.junit.jupiter.api.Test;
import org.springframework.mock.web.MockHttpServletRequest;
import org.springframework.mock.web.MockHttpServletResponse;

final class SpringInterceptorPayeeMiddlewareTest {
    @Test void registersOnlyExplicitlyConfiguredRoutes() {
        VaultTransport transport = new NoopTransport();
        var core = new PayeeMiddleware(transport, PayeeConfig.defaults());
        assertNotNull(core.spring(List.of(SpringInterceptorPayeeMiddleware.Route.mcpTool(
                "/mcp", "marketpulse.realtime_news", "scenario-1", "offer-1",
                "mcp://marketpulse/realtime", "https://marketpulse.test"))));
    }

    @Test void bypassesAFreeToolOnTheSharedMcpRoute() throws Exception {
        var core = new PayeeMiddleware(new NoopTransport(), PayeeConfig.defaults());
        var filter = core.spring(List.of(SpringInterceptorPayeeMiddleware.Route.mcpTool(
                "/mcp", "marketpulse.realtime_news", "scenario-1", "offer-1",
                "mcp://marketpulse/realtime", "https://marketpulse.test")));
        var request = new MockHttpServletRequest("POST", "/mcp");
        request.setContent(("{\"method\":\"tools/call\",\"params\":{\"name\":"
                + "\"marketpulse.yesterday_news\"}}").getBytes(java.nio.charset.StandardCharsets.UTF_8));
        var response = new MockHttpServletResponse();
        var passed = new AtomicBoolean();
        filter.doFilter(request, response, (_request, _response) -> passed.set(true));
        assertTrue(passed.get());
    }

    private static final class NoopTransport implements VaultTransport {
        @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
        @Override public <T> T post(String path, Object body, String key, Class<T> type) {
            throw new UnsupportedOperationException();
        }
    }
}
