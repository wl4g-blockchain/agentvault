package io.agentvault.sdk.transport;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assertions.assertThrows;
import io.agentvault.sdk.config.Config.MqttConfig;
import java.time.Duration;
import org.junit.jupiter.api.Test;

final class MqttVaultTransportTest {
    @Test void rejectsTopicInjectionAndRedactsCredentials() {
        assertThrows(IllegalArgumentException.class, () -> new MqttConfig(
                "localhost", 1883, false, "user", "pass", "worker", "a/../b", Duration.ofSeconds(1), 1024));
        var config = new MqttConfig(
                "localhost", 1883, false, "secret-user", "secret-pass", "worker", "client-a",
                Duration.ofSeconds(1), 1024);
        assertFalse(config.toString().contains("secret-user"));
        assertFalse(config.toString().contains("secret-pass"));
    }

    @Test void requiresExactQosOneSubscriptionGrants() {
        assertTrue(MqttVaultTransport.subscriptionGranted(new int[]{1}, 1));
        assertFalse(MqttVaultTransport.subscriptionGranted(new int[]{128}, 1));
        assertFalse(MqttVaultTransport.subscriptionGranted(new int[]{1, 1}, 1));
    }
}
