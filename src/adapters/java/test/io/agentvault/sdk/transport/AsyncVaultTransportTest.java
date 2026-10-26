package io.agentvault.sdk.transport;

import static org.junit.jupiter.api.Assertions.assertTrue;
import org.junit.jupiter.api.Test;

final class AsyncVaultTransportTest {
    @Test void mqttExposesTheNativeAsynchronousContract() {
        assertTrue(AsyncVaultTransport.class.isAssignableFrom(MqttVaultTransport.class));
    }
}
