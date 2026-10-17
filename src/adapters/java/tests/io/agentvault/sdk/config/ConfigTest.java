package io.agentvault.sdk.config;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;

import io.agentvault.sdk.config.Config.PayeeConfig;
import io.agentvault.sdk.config.Config.PayerConfig;
import org.junit.jupiter.api.Test;

final class ConfigTest {
    @Test void appliesSafeRoleDefaultsAndRejectsInvalidLimits() {
        var payer = PayerConfig.defaults("scenario");
        assertEquals(0, payer.deliveryRetries());
        assertEquals(1024 * 1024, PayeeConfig.defaults().maxBodyBytes());
        assertThrows(IllegalArgumentException.class, () -> new PayeeConfig(0));
    }
}
