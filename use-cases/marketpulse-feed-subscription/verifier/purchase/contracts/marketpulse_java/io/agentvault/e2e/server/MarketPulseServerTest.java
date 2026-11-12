package io.agentvault.e2e.server;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import io.agentvault.e2e.handler.FeedHandler;
import org.junit.jupiter.api.Test;
import org.springframework.web.bind.annotation.RestController;

final class MarketPulseServerTest {
    @Test void validatesListenerAndOwnsMcpEndpoint() throws Exception {
        assertEquals("127.0.0.1", MarketPulseServer.bindHost("127.0.0.1:8081"));
        assertEquals(8081, MarketPulseServer.bindPort("127.0.0.1:8081"));
        assertThrows(IllegalArgumentException.class, () -> MarketPulseServer.bindPort("invalid"));
        assertTrue(MarketPulseServer.McpEndpoint.class.isAnnotationPresent(RestController.class));
        assertNotNull(MarketPulseServer.McpEndpoint.class.getConstructor(FeedHandler.class));
        assertNotNull(MarketPulseServer.McpEndpoint.class.getDeclaredMethod(
                "mcp", jakarta.servlet.http.HttpServletRequest.class, byte[].class));
        assertThrows(
                NoSuchMethodException.class,
                () -> MarketPulseServer.class.getDeclaredMethod("mcpEndpoint", FeedHandler.class));
    }
}
