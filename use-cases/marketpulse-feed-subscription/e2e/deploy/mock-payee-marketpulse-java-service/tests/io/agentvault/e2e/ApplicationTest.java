package io.agentvault.e2e;

import static org.junit.jupiter.api.Assertions.assertNotNull;

import org.junit.jupiter.api.Test;

final class ApplicationTest {
    @Test void isAThinSpringBootEntrypoint() throws Exception {
        assertNotNull(Application.class.getDeclaredMethod("main", String[].class));
    }
}
