package io.agentvault.sdk.transport;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import io.agentvault.sdk.model.Base.ApiException;
import io.agentvault.sdk.model.Base.VaultTransport;
import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.time.Duration;

public final class HttpVaultTransport implements VaultTransport {
    private static final ObjectMapper JSON = new ObjectMapper();
    private final URI baseUri;
    private final String authguardBearer;
    private final Duration timeout;
    private final int maxResponseBytes;
    private final HttpClient client;

    public HttpVaultTransport(String baseUrl) {
        this(baseUrl, null, Duration.ofSeconds(30), 2 * 1024 * 1024);
    }

    public HttpVaultTransport(String baseUrl, String authguardBearer) {
        this(baseUrl, authguardBearer, Duration.ofSeconds(30), 2 * 1024 * 1024);
    }

    public HttpVaultTransport(String baseUrl, String authguardBearer, Duration timeout, int maxResponseBytes) {
        this.baseUri = URI.create(baseUrl);
        if (!("http".equals(baseUri.getScheme()) || "https".equals(baseUri.getScheme())) || baseUri.getHost() == null
                || baseUri.getUserInfo() != null || baseUri.getQuery() != null || baseUri.getFragment() != null
                || (authguardBearer != null && (authguardBearer.isBlank()
                    || authguardBearer.chars().anyMatch(Character::isISOControl)))
                || timeout == null || timeout.isZero() || timeout.isNegative() || maxResponseBytes <= 0) {
            throw new IllegalArgumentException("unsafe AgentVault transport configuration");
        }
        this.authguardBearer = authguardBearer;
        this.timeout = timeout;
        this.maxResponseBytes = maxResponseBytes;
        this.client = HttpClient.newBuilder()
                .followRedirects(HttpClient.Redirect.NEVER)
                .connectTimeout(timeout)
                .build();
    }

    @Override public <T> T get(String path, Class<T> type) {
        return request("GET", path, null, null, type);
    }

    @Override public <T> T post(String path, Object body, String idempotencyKey, Class<T> type) {
        return request("POST", path, body, idempotencyKey, type);
    }

    private <T> T request(String method, String path, Object body, String idempotencyKey, Class<T> type) {
        if (!path.startsWith("/") || path.startsWith("//") || path.contains("\\")) {
            throw new IllegalArgumentException("path must be origin-relative");
        }
        URI target = URI.create(baseUri.getScheme() + "://" + baseUri.getAuthority() + path);
        try {
            HttpRequest.Builder builder = HttpRequest.newBuilder(target)
                    .timeout(timeout)
                    .header("Accept", "application/json");
            if (authguardBearer != null) builder.header("Authorization", "Bearer " + authguardBearer);
            if (idempotencyKey != null) builder.header("Idempotency-Key", idempotencyKey);
            if (body == null) {
                builder.GET();
            } else {
                builder.header("Content-Type", "application/json")
                        .method(method, HttpRequest.BodyPublishers.ofByteArray(JSON.writeValueAsBytes(body)));
            }
            HttpResponse<java.io.InputStream> response = client.send(
                    builder.build(), HttpResponse.BodyHandlers.ofInputStream());
            byte[] raw;
            try (var stream = response.body()) {
                raw = stream.readNBytes(maxResponseBytes + 1);
            }
            if (raw.length > maxResponseBytes) {
                throw new IllegalStateException("AgentVault response exceeds configured limit");
            }
            JsonNode value = raw.length == 0 ? JSON.nullNode() : JSON.readTree(raw);
            if (response.statusCode() < 200 || response.statusCode() >= 300) {
                JsonNode error = value.path("error");
                throw new ApiException(response.statusCode(), error.path("code").asText("HTTP_ERROR"),
                        error.path("message").asText("AgentVault returned HTTP " + response.statusCode()));
            }
            return JSON.treeToValue(value, type);
        } catch (IOException error) {
            throw new IllegalStateException("AgentVault HTTP I/O failed", error);
        } catch (InterruptedException error) {
            Thread.currentThread().interrupt();
            throw new IllegalStateException("AgentVault HTTP request interrupted", error);
        }
    }

    @Override public String toString() {
        return "HttpVaultTransport[origin=" + baseUri.getScheme() + "://" + baseUri.getAuthority()
                + ", authguardBearer=" + (authguardBearer == null ? "disabled" : "<redacted>") + "]";
    }
}
