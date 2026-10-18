package io.agentvault.sdk.transport;

import static org.junit.jupiter.api.Assertions.assertEquals;
import org.junit.jupiter.api.Test;

final class VaultTransportTest {
    @Test void apiExceptionPreservesStatusAndCode() {
        var error = new VaultTransport.ApiException(409, "RUN_CONFLICT", "conflict");
        assertEquals(409, error.status());
        assertEquals("RUN_CONFLICT", error.code());
    }
}
