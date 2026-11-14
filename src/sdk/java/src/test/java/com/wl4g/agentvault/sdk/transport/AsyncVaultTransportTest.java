package com.wl4g.agentvault.sdk.transport;

import static org.junit.jupiter.api.Assertions.assertTrue;

import org.junit.jupiter.api.Test;

final class AsyncVaultTransportTest {

    @Test
    void httpExposesTheNativeAsynchronousContract() {
        assertTrue(AsyncVaultTransport.class.isAssignableFrom(HttpVaultTransport.class));
    }
}
