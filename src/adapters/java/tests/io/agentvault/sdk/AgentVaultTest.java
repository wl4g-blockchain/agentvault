package io.agentvault.sdk;

import static org.junit.jupiter.api.Assertions.assertEquals;
import io.agentvault.sdk.config.Config.PayeeConfig;
import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.transport.VaultTransport;
import org.junit.jupiter.api.Test;

final class AgentVaultTest {
    @Test void createsOnlyRoleOrientedMiddlewareEntrypoints() {
        var vault = new AgentVault(new NoopTransport());
        assertEquals("PayerMiddleware", vault.payer(PayerConfig.defaults("scenario-1"))
                .getClass().getSimpleName());
        assertEquals("PayeeMiddleware", vault.payee(PayeeConfig.defaults()).getClass().getSimpleName());
    }

    private static final class NoopTransport implements VaultTransport {
        @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
        @Override public <T> T post(String path, Object body, Class<T> type) {
            throw new UnsupportedOperationException();
        }
    }
}
