package io.agentvault.sdk.config;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import io.agentvault.sdk.config.Config.PayeeConfig;
import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.model.Base.AssetIdentity;
import org.junit.jupiter.api.Test;

final class ConfigTest {
    @Test void appliesSafeRoleDefaultsAndRejectsInvalidLimits() {
        var payer = PayerConfig.defaults("scenario", "wallet", "x402",
                new AssetIdentity("eip155:1", "USDC", null, 6), "100", "recipient");
        assertEquals(0, payer.deliveryRetries());
        assertEquals(1024 * 1024, PayeeConfig.defaults().maxBodyBytes());
        assertThrows(IllegalArgumentException.class, () -> new PayeeConfig(0));
    }
}
