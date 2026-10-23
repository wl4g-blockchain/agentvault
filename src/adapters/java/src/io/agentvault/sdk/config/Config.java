package io.agentvault.sdk.config;

import java.net.InetAddress;
import java.time.Duration;
import java.util.List;

public final class Config {
    private Config() {}

    public record PayerConfig(
            String scenarioId,
            Duration approvalTimeout,
            Duration approvalPollInterval,
            int deliveryRetries,
            int maxChallengeBytes,
            List<String> protocols) {
        public PayerConfig(
                String scenarioId,
                Duration approvalTimeout,
                Duration approvalPollInterval,
                int deliveryRetries,
                int maxChallengeBytes) {
            this(scenarioId, approvalTimeout, approvalPollInterval,
                    deliveryRetries, maxChallengeBytes, List.of("x402", "mpp"));
        }

        public PayerConfig {
            require("scenarioId", scenarioId);
            protocols = validateProtocols(protocols);
            if (approvalTimeout == null || approvalTimeout.isZero() || approvalTimeout.isNegative()
                    || approvalPollInterval == null || approvalPollInterval.isZero() || approvalPollInterval.isNegative()
                    || approvalPollInterval.compareTo(approvalTimeout) > 0 || deliveryRetries < 0
                    || deliveryRetries > 3 || maxChallengeBytes <= 0) {
                throw new IllegalArgumentException("invalid payer middleware limits");
            }
        }

        public static PayerConfig defaults(String scenarioId) {
            return new PayerConfig(scenarioId,
                    Duration.ofMinutes(2), Duration.ofSeconds(1), 0, 256 * 1024,
                    List.of("x402", "mpp"));
        }
    }

    public record PayeeConfig(int maxBodyBytes, List<String> protocols) {
        public PayeeConfig(int maxBodyBytes) {
            this(maxBodyBytes, List.of("x402", "mpp"));
        }

        public PayeeConfig {
            if (maxBodyBytes <= 0) throw new IllegalArgumentException("payee middleware body limit must be positive");
            protocols = validateProtocols(protocols);
        }

        public static PayeeConfig defaults() {
            return new PayeeConfig(1024 * 1024, List.of("x402", "mpp"));
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

    public record VaultTransportConfig(String provider, GrpcConfig grpc, MqttConfig mqtt) {
        public VaultTransportConfig {
            boolean valid = "grpc".equals(provider) ? grpc != null && mqtt == null
                    : "mqtt".equals(provider) && mqtt != null && grpc == null;
            if (!valid) {
                throw new IllegalArgumentException(
                        "transport provider requires exactly one matching configuration block");
            }
        }

        public static VaultTransportConfig grpc(GrpcConfig config) {
            return new VaultTransportConfig("grpc", config, null);
        }

        public static VaultTransportConfig mqtt(MqttConfig config) {
            return new VaultTransportConfig("mqtt", null, config);
        }
    }

    private static void require(String name, String value) {
        if (value == null || value.isBlank()) throw new IllegalArgumentException(name + " cannot be blank");
    }

    private static List<String> validateProtocols(List<String> protocols) {
        if (protocols == null || protocols.isEmpty()
                || protocols.stream().distinct().count() != protocols.size()
                || protocols.stream().anyMatch(value -> value == null || !value.matches("[a-z0-9-]{1,32}"))) {
            throw new IllegalArgumentException("payment protocols must be unique safe lowercase names");
        }
        return List.copyOf(protocols);
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
