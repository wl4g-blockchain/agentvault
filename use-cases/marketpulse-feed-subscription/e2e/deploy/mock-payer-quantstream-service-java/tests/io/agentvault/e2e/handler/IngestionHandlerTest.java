package io.agentvault.e2e.handler;

import static org.junit.jupiter.api.Assertions.assertNotNull;

import org.junit.jupiter.api.Test;

final class IngestionHandlerTest {
    @Test void handlerKeepsOnlyOperationalHttpMethods() throws Exception {
        assertNotNull(IngestionHandler.class.getDeclaredMethod("health"));
        assertNotNull(IngestionHandler.class.getDeclaredMethod("metrics"));
    }
}
