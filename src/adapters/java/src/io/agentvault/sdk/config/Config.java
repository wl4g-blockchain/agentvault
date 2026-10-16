package io.agentvault.sdk.config;

import io.agentvault.sdk.model.Base.AssetIdentity;
import java.net.InetAddress;
import java.time.Duration;

public final class Config {
    private Config() {}

    public record PayerConfig(
            String scenarioId,
            String walletId,
            String protocol,
            AssetIdentity asset,
            String amount,
            String recipient,
            Duration approvalTimeout,
            Duration approvalPollInterval,
            int deliveryRetries,
            int maxChallengeBytes) {
        public PayerConfig {
            require("scenarioId", scenarioId);
            require("walletId", walletId);
            require("protocol", protocol);
            require("amount", amount);
            require("recipient", recipient);
            if (asset == null || approvalTimeout == null || approvalTimeout.isZero() || approvalTimeout.isNegative()
                    || approvalPollInterval == null || approvalPollInterval.isZero() || approvalPollInterval.isNegative()
                    || approvalPollInterval.compareTo(approvalTimeout) > 0 || deliveryRetries < 0
                    || deliveryRetries > 3 || maxChallengeBytes <= 0) {
                throw new IllegalArgumentException("invalid payer middleware limits");
            }
        }

        public static PayerConfig defaults(
                String scenarioId,
                String walletId,
                String protocol,
                AssetIdentity asset,
                String amount,
                String recipient) {
            return new PayerConfig(scenarioId, walletId, protocol, asset, amount, recipient,
                    Duration.ofMinutes(2), Duration.ofSeconds(1), 0, 256 * 1024);
        }
    }

    public record PayeeConfig(int maxBodyBytes) {
        public PayeeConfig {
            if (maxBodyBytes <= 0) throw new IllegalArgumentException("payee middleware body limit must be positive");
        }

        public static PayeeConfig defaults() {
            return new PayeeConfig(1024 * 1024);
        }
    }

    public record GrpcConfig(
            String endpoint,
            boolean tls,
            String authguardBearer,
            Duration timeout,
            int maxMessageBytes) {
        public GrpcConfig {
            if (timeout == null) timeout = Duration.ofSeconds(30);
            if (maxMessageBytes == 0) maxMessageBytes = 2 * 1024 * 1024;
            String host = endpointHost(endpoint);
            if (host == null || timeout.isZero() || timeout.isNegative() || maxMessageBytes <= 0
                    || (!tls && !loopback(host))
                    || (authguardBearer != null && (authguardBearer.isBlank()
                        || authguardBearer.chars().anyMatch(value -> value < 0x20 || value == 0x7f)))) {
                throw new IllegalArgumentException("invalid or unsafe AgentVault gRPC configuration");
            }
        }

        public static GrpcConfig secure(String endpoint) {
            return new GrpcConfig(endpoint, true, null, Duration.ofSeconds(30), 2 * 1024 * 1024);
        }

        @Override public String toString() {
            return "GrpcConfig[endpoint=" + endpoint + ", tls=" + tls + ", authguardBearer="
                    + (authguardBearer == null ? "disabled" : "<redacted>") + "]";
        }
    }

    public record MqttConfig(
            String host,
            int port,
            boolean tls,
            String username,
            String password,
            String connectionPrefix,
            String vaultClientId,
            Duration timeout,
            int maxResponseBytes) {
        public MqttConfig {
            if (host == null || host.isBlank() || port <= 0 || port > 65_535 || !identifier(username)
                    || password == null || password.isEmpty() || !identifier(connectionPrefix)
                    || !identifier(vaultClientId) || timeout == null || timeout.isZero() || timeout.isNegative()
                    || maxResponseBytes <= 0 || (!tls && !loopback(host))) {
                throw new IllegalArgumentException("invalid or unsafe AgentVault MQTT configuration");
            }
        }

        public static MqttConfig defaults(
                String host,
                int port,
                boolean tls,
                String username,
                String password,
                String connectionPrefix,
                String vaultClientId) {
            return new MqttConfig(host, port, tls, username, password, connectionPrefix, vaultClientId,
                    Duration.ofSeconds(30), 2 * 1024 * 1024);
        }

        @Override public String toString() {
            return "MqttConfig[host=" + host + ", port=" + port + ", tls=" + tls
                    + ", credentials=<redacted>, connectionPrefix=" + connectionPrefix
                    + ", vaultClientId=" + vaultClientId + "]";
        }
    }

    private static void require(String name, String value) {
        if (value == null || value.isBlank()) throw new IllegalArgumentException(name + " cannot be blank");
    }

    private static boolean identifier(String value) {
        return value != null && value.matches("[A-Za-z0-9_.-]{1,128}");
    }

    private static String endpointHost(String endpoint) {
        if (endpoint == null) return null;
        try {
            var uri = java.net.URI.create("http://" + endpoint);
            return uri.getHost() == null || uri.getPort() <= 0 || uri.getPort() > 65_535 ? null : uri.getHost();
        } catch (IllegalArgumentException ignored) {
            return null;
        }
    }

    private static boolean loopback(String host) {
        if ("localhost".equalsIgnoreCase(host)) return true;
        try {
            return InetAddress.getByName(host).isLoopbackAddress();
        } catch (java.net.UnknownHostException ignored) {
            return false;
        }
    }
}
