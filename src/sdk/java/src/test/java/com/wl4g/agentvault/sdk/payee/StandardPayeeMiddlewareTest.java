package com.wl4g.agentvault.sdk.payee;

import static org.junit.jupiter.api.Assertions.assertNotNull;
import com.wl4g.agentvault.sdk.config.Config.PayeeConfig;
import com.wl4g.agentvault.sdk.transport.VaultTransport;
import org.junit.jupiter.api.Test;

final class StandardPayeeMiddlewareTest {
    @Test void createsTheFrameworkNeutralAdapter() {
        VaultTransport transport = new NoopTransport();
        assertNotNull(new PayeeMiddleware(transport, PayeeConfig.defaults()).standard());
    }

    private static final class NoopTransport implements VaultTransport {
        @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
        @Override public <T> T post(String path, Object body, Class<T> type) {
            throw new UnsupportedOperationException();
        }
    }
}
