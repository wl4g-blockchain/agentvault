package io.agentvault.sdk.transport;

import io.agentvault.sdk.config.Config.VaultTransportConfig;

/** Creates one fresh owned transport provider per typed configuration. */
public final class VaultTransportFactory {
    public VaultTransport create(VaultTransportConfig config) {
        java.util.Objects.requireNonNull(config, "config");
        return switch (config.provider()) {
            case "grpc" -> new GrpcVaultTransport(config.grpc());
            case "mqtt" -> new MqttVaultTransport(config.mqtt());
            default -> throw new IllegalArgumentException("unsupported AgentVault transport provider");
        };
    }
}
