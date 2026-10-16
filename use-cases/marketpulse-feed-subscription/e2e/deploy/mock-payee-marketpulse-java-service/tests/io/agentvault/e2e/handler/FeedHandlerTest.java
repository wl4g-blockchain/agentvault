package io.agentvault.e2e.handler;

import static org.junit.jupiter.api.Assertions.assertNotNull;

import org.junit.jupiter.api.Test;

final class FeedHandlerTest {
    @Test void ownsOperationalAndMcpBusinessOperations() throws Exception {
        assertNotNull(FeedHandler.class.getDeclaredMethod("health"));
        assertNotNull(FeedHandler.class.getDeclaredMethod("metrics"));
        assertNotNull(FeedHandler.class.getDeclaredMethod("mcp", jakarta.servlet.http.HttpServletRequest.class, byte[].class));
    }
}
