package com.wl4g.agentvault.sdk.config;

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
        List<String> protocols
    ) {
        public PayerConfig(
            String scenarioId,
            Duration approvalTimeout,
            Duration approvalPollInterval,
            int deliveryRetries,
            int maxChallengeBytes
        ) {
            this(
                scenarioId,
                approvalTimeout,
                approvalPollInterval,
                deliveryRetries,
                maxChallengeBytes,
                List.of("x402", "mpp")
            );
        }

        public PayerConfig {
            require("scenarioId", scenarioId);
            protocols = validateProtocols(protocols);
            if (
                approvalTimeout == null ||
                approvalTimeout.isZero() ||
                approvalTimeout.isNegative() ||
                approvalPollInterval == null ||
                approvalPollInterval.isZero() ||
                approvalPollInterval.isNegative() ||
                approvalPollInterval.compareTo(approvalTimeout) > 0 ||
                deliveryRetries < 0 ||
                deliveryRetries > 3 ||
                maxChallengeBytes <= 0
            ) {
                throw new IllegalArgumentException("invalid payer middleware limits");
            }
        }

        public static PayerConfig defaults(String scenarioId) {
            return new PayerConfig(
                scenarioId,
                Duration.ofMinutes(2),
                Duration.ofSeconds(1),
                0,
                256 * 1024,
                List.of("x402", "mpp")
            );
        }
    }

    public record PayeeConfig(int maxBodyBytes, List<String> protocols) {
        public PayeeConfig(int maxBodyBytes) {
            this(maxBodyBytes, List.of("x402", "mpp"));
        }

        public PayeeConfig {
            if (maxBodyBytes <= 0) throw new IllegalArgumentException(
                "payee middleware body limit must be positive"
            );
            protocols = validateProtocols(protocols);
        }

        public static PayeeConfig defaults() {
            return new PayeeConfig(1024 * 1024, List.of("x402", "mpp"));
        }
    }

    public record HttpConfig(String baseUrl, String authguardBearer, Duration timeout, int maxResponseBytes) {
        public HttpConfig {
            if (timeout == null) timeout = Duration.ofSeconds(30);
            if (maxResponseBytes == 0) maxResponseBytes = 2 * 1024 * 1024;
            var url = java.net.URI.create(baseUrl);
            if (
                !java.util.Set.of("http", "https").contains(url.getScheme()) ||
                url.getHost() == null ||
                url.getUserInfo() != null ||
                url.getQuery() != null ||
                url.getFragment() != null ||
                (!url.getPath().isEmpty() && !url.getPath().equals("/")) ||
                timeout.isZero() ||
                timeout.isNegative() ||
                maxResponseBytes <= 0 ||
                (authguardBearer != null && authguardBearer.chars().anyMatch(c -> c < 32 || c == 127))
            ) throw new IllegalArgumentException("invalid AgentVault HTTP configuration");
        }

        public static HttpConfig defaults(String baseUrl) {
            return new HttpConfig(baseUrl, null, Duration.ofSeconds(30), 2 * 1024 * 1024);
        }

        @Override
        public String toString() {
            return "HttpConfig[baseUrl=" + baseUrl + ", authguardBearer=<redacted>]";
        }
    }

    public record VaultTransportConfig(String provider, HttpConfig http) {
        public VaultTransportConfig {
            if (!"http".equals(provider) || http == null) throw new IllegalArgumentException(
                "transport provider must be http"
            );
        }

        public static VaultTransportConfig http(HttpConfig config) {
            return new VaultTransportConfig("http", config);
        }
    }

    private static void require(String name, String value) {
        if (value == null || value.isBlank()) throw new IllegalArgumentException(name + " cannot be blank");
    }

    private static List<String> validateProtocols(List<String> protocols) {
        if (
            protocols == null ||
            protocols.isEmpty() ||
            protocols.stream().distinct().count() != protocols.size() ||
            protocols.stream().anyMatch(value -> value == null || !value.matches("[a-z0-9-]{1,32}"))
        ) {
            throw new IllegalArgumentException("payment protocols must be unique safe lowercase names");
        }
        return List.copyOf(protocols);
    }
}
