package io.agentvault.sdk;

import static org.junit.jupiter.api.Assertions.assertEquals;
import io.agentvault.sdk.config.Config.PayeeConfig;
import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.model.Base.AssetIdentity;
import io.agentvault.sdk.model.Base.VaultTransport;
import org.junit.jupiter.api.Test;

final class AgentVaultTest {
    @Test void createsOnlyRoleOrientedMiddlewareEntrypoints() {
        var vault = new AgentVault(new NoopTransport());
        assertEquals("PayerMiddleware", vault.payer(PayerConfig.defaults(
                "scenario-1", "wallet-1", "x402",
                new AssetIdentity("eip155:1", "USDC", null, 6), "100", "recipient-1"))
                .getClass().getSimpleName());
        assertEquals("PayeeMiddleware", vault.payee(PayeeConfig.defaults()).getClass().getSimpleName());
    }

    private static final class NoopTransport implements VaultTransport {
        @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
        @Override public <T> T post(String path, Object body, String key, Class<T> type) {
            throw new UnsupportedOperationException();
        }
    }
}
