package io.agentvault.sdk.protocol;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.util.List;
import org.junit.jupiter.api.Test;

final class ProtocolRegistryTest {
    @Test void exposesOnlyConfiguredProviders() {
        var registry = new PaymentProtocolFactory().create(List.of("mpp"));
        assertEquals("mpp", registry.get("mpp").name());
        assertThrows(IllegalArgumentException.class, () -> registry.get("x402"));
    }
}
