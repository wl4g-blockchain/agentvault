package io.agentvault.sdk.transport;

import static org.junit.jupiter.api.Assertions.assertDoesNotThrow;
import static org.junit.jupiter.api.Assertions.assertThrows;
import io.agentvault.sdk.config.Config.GrpcConfig;
import java.time.Duration;
import org.junit.jupiter.api.Test;

final class GrpcVaultTransportTest {
    @Test void rejectsCleartextNonLoopbackEndpoints() {
        assertThrows(IllegalArgumentException.class,
                () -> new GrpcConfig("vault.example:50051", false, null, Duration.ofSeconds(1), 1024));
        assertDoesNotThrow(
                () -> new GrpcConfig("127.0.0.1:50051", false, null, Duration.ofSeconds(1), 1024));
    }
}
