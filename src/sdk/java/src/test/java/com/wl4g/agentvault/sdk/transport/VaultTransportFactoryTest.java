package com.wl4g.agentvault.sdk.transport;

import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNotSame;

import com.wl4g.agentvault.sdk.config.Config.HttpConfig;
import com.wl4g.agentvault.sdk.config.Config.VaultTransportConfig;
import java.time.Duration;
import org.junit.jupiter.api.Test;

final class VaultTransportFactoryTest {

    @Test
    void createsIsolatedIdempotentlyClosableProviders() {
        var config = VaultTransportConfig.http(
            new HttpConfig("http://127.0.0.1:65535", null, Duration.ofSeconds(1), 1024)
        );
        var factory = new VaultTransportFactory();
        VaultTransport first = factory.create(config);
        VaultTransport second = factory.create(config);
        assertInstanceOf(HttpVaultTransport.class, first);
        assertInstanceOf(HttpVaultTransport.class, second);
        assertNotSame(first, second);
        first.close();
        first.close();
        second.close();
    }
}
