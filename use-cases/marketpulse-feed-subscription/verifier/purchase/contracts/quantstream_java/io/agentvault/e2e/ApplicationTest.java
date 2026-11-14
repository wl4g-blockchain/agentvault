package io.agentvault.e2e;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;
import org.springframework.boot.WebApplicationType;

final class ApplicationTest {
    @Test void rejectsUndeclaredArgumentsBeforeRuntimeStartup() {
        assertThrows(IllegalArgumentException.class, () -> Application.main(new String[] {"--unknown"}));
    }

    @Test void oneShotModeDoesNotOpenACompetingHttpListener() {
        assertEquals(WebApplicationType.NONE, Application.webApplicationType(new String[] {"--once"}));
        assertEquals(WebApplicationType.REACTIVE, Application.webApplicationType(new String[0]));
        assertTrue(Application.application(new String[] {"--once"}).getAdditionalProfiles().contains("oneshot"));
        assertFalse(Application.application(new String[0]).getAdditionalProfiles().contains("oneshot"));
    }
}
