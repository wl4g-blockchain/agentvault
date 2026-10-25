package io.agentvault.sdk.protocol;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNotSame;
import static org.junit.jupiter.api.Assertions.assertThrows;

import java.util.List;
import org.junit.jupiter.api.Test;

final class PaymentProtocolFactoryTest {
    @Test void createsIsolatedRegistriesAndRejectsDuplicates() {
        var factory = new PaymentProtocolFactory();
        assertEquals("x402", factory.create(List.of("x402", "mpp")).get("x402").name());
        assertNotSame(factory.create(List.of("mpp")).get("mpp"), factory.create(List.of("mpp")).get("mpp"));
        assertThrows(IllegalArgumentException.class, () -> factory.create(List.of("x402", "x402")));
    }
}
