package io.agentvault.e2e.route;

import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;

import org.junit.jupiter.api.Test;

final class FeedControllerTest {
    @Test void exposesOnlyOperationalRoutes() throws Exception {
        assertNotNull(FeedController.class.getDeclaredMethod("health"));
        assertNotNull(FeedController.class.getDeclaredMethod("metrics"));
        assertThrows(NoSuchMethodException.class, () -> FeedController.class.getDeclaredMethod("mcp"));
    }
}
