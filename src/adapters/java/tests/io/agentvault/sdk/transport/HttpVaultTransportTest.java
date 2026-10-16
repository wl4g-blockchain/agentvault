package io.agentvault.sdk.transport;

import static org.junit.jupiter.api.Assertions.assertFalse;
import org.junit.jupiter.api.Test;

final class HttpVaultTransportTest {
    @Test void redactsAuthguardBearer() {
        assertFalse(new HttpVaultTransport("https://vault.test", "bearer-secret").toString()
                .contains("bearer-secret"));
    }
}
