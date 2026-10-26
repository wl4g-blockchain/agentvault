package io.agentvault.sdk.transport;

import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNotSame;

import io.agentvault.sdk.config.Config.GrpcConfig;
import io.agentvault.sdk.config.Config.VaultTransportConfig;
import java.time.Duration;
import org.junit.jupiter.api.Test;

final class VaultTransportFactoryTest {
    @Test void createsIsolatedIdempotentlyClosableProviders() {
        var config = VaultTransportConfig.grpc(new GrpcConfig(
                "127.0.0.1:65535", false, null, Duration.ofSeconds(1), 1024));
        var factory = new VaultTransportFactory();
        VaultTransport first = factory.create(config);
        VaultTransport second = factory.create(config);
        assertInstanceOf(GrpcVaultTransport.class, first);
        assertInstanceOf(GrpcVaultTransport.class, second);
        assertNotSame(first, second);
        first.close();
        first.close();
        second.close();
    }
}
