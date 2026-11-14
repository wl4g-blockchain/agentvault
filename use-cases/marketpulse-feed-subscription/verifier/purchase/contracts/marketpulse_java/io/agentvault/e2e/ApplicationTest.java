package io.agentvault.e2e;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

final class ApplicationTest {
    @Test void isAThinSpringBootEntrypoint() throws Exception {
        assertNotNull(Application.class.getDeclaredMethod("main", String[].class));
    }

    @Test void recognizesOnlyTheDedicatedHealthcheckCommand() {
        assertTrue(Application.healthcheckRequested(new String[] {"--healthcheck"}));
        assertFalse(Application.healthcheckRequested(new String[] {}));
        assertFalse(Application.healthcheckRequested(new String[] {"--healthcheck", "extra"}));
    }
}
