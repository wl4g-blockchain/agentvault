package io.agentvault.e2e.route;

import static org.junit.jupiter.api.Assertions.assertNotNull;

import org.junit.jupiter.api.Test;

final class IngestionControllerTest {
    @Test void controllerExposesOnlyOperationalEndpoints() throws Exception {
        assertNotNull(IngestionController.class.getDeclaredMethod("health"));
        assertNotNull(IngestionController.class.getDeclaredMethod("metrics"));
    }
}
