package com.wl4g.agentvault.sdk.transport;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.wl4g.agentvault.sdk.config.Config.HttpConfig;
import java.io.ByteArrayOutputStream;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.nio.ByteBuffer;
import java.util.List;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionStage;
import java.util.concurrent.Flow;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.atomic.AtomicBoolean;

/** Pooled HTTP transport with bounded bodies and cancellable native futures. */
public final class HttpVaultTransport implements AsyncVaultTransport {

    private static final ObjectMapper JSON = new ObjectMapper();
    private final HttpConfig config;
    private final HttpClient client;
    private final AtomicBoolean closed = new AtomicBoolean();
    private final java.util.Set<CompletableFuture<?>> pending =
        java.util.concurrent.ConcurrentHashMap.newKeySet();

    public HttpVaultTransport(HttpConfig config) {
        this.config = config;
        this.client = HttpClient.newBuilder()
            .connectTimeout(config.timeout())
            .followRedirects(HttpClient.Redirect.NEVER)
            .build();
    }

    @Override
    public String toString() {
        return "HttpVaultTransport[config=" + config + "]";
    }

    @Override
    public <T> T get(String path, Class<T> type) {
        return await(getAsync(path, type));
    }

    @Override
    public <T> T post(String path, Object body, Class<T> type) {
        return await(postAsync(path, body, type));
    }

    @Override
    public <T> CompletableFuture<T> getAsync(String path, Class<T> type) {
        return execute("GET", path, null, type);
    }

    @Override
    public <T> CompletableFuture<T> postAsync(String path, Object body, Class<T> type) {
        return execute("POST", path, body, type);
    }

    private static <T> T await(CompletableFuture<T> future) {
        try {
            return future.join();
        } catch (java.util.concurrent.CompletionException failure) {
            if (failure.getCause() instanceof RuntimeException cause) throw cause;
            throw failure;
        }
    }

    private <T> CompletableFuture<T> execute(String method, String path, Object body, Class<T> type) {
        if (closed.get()) return CompletableFuture.failedFuture(
            new IllegalStateException("AgentVault HTTP transport is closed")
        );
        if (!path.startsWith("/api/v1/") || path.contains("\\")) return CompletableFuture.failedFuture(
            new IllegalArgumentException("invalid AgentVault HTTP request")
        );
        final byte[] payload;
        try {
            payload = body == null ? new byte[0] : JSON.writeValueAsBytes(body);
        } catch (Exception e) {
            return CompletableFuture.failedFuture(
                new IllegalArgumentException("encode AgentVault request", e)
            );
        }
        if (payload.length > config.maxResponseBytes()) return CompletableFuture.failedFuture(
            new IllegalArgumentException("AgentVault HTTP request exceeds configured limit")
        );
        String traceId = java.util.UUID.randomUUID().toString().replace("-", ""),
            spanId = java.util.UUID.randomUUID().toString().replace("-", "").substring(0, 16);
        var request = HttpRequest.newBuilder(URI.create(config.baseUrl().replaceAll("/$", "") + path))
            .timeout(config.timeout())
            .header("accept", "application/json")
            .header("traceparent", "00-" + traceId + "-" + spanId + "-01");
        if (config.authguardBearer() != null) request.header(
            "authorization",
            "Bearer " + config.authguardBearer()
        );
        if (body != null) request.header("content-type", "application/json");
        java.util.logging.Logger.getLogger(HttpVaultTransport.class.getName()).info(
            "agentvault.sdk.transport.http.produced trace_id=" +
                traceId +
                " span_id=" +
                spanId +
                " parent_id="
        );
        var call = client.sendAsync(
            request.method(method, HttpRequest.BodyPublishers.ofByteArray(payload)).build(),
            info -> new LimitedBody(config.maxResponseBytes())
        );
        pending.add(call);
        var deadline = new CompletableFuture<HttpResponse<byte[]>>();
        call.whenComplete((response, error) -> {
            if (error == null) deadline.complete(response);
            else deadline.completeExceptionally(error);
        });
        var result = deadline
            .orTimeout(config.timeout().toMillis(), TimeUnit.MILLISECONDS)
            .thenApply(response -> {
                try {
                    if (response.statusCode() < 200 || response.statusCode() >= 300) {
                        com.fasterxml.jackson.databind.JsonNode envelope = JSON.createObjectNode();
                        try {
                            var decoded = JSON.readTree(response.body());
                            if (decoded != null) envelope = decoded.path("error");
                        } catch (java.io.IOException ignored) {
                            // Preserve the HTTP status when a proxy returns a non-JSON error.
                        }
                        throw new VaultTransport.ApiException(
                            response.statusCode(),
                            envelope.path("code").asText("TRANSPORT_ERROR"),
                            envelope.path("message").asText("AgentVault HTTP request failed")
                        );
                    }
                    return JSON.readValue(response.body(), type);
                } catch (VaultTransport.ApiException e) {
                    throw e;
                } catch (Exception e) {
                    throw new IllegalStateException("invalid AgentVault HTTP response", e);
                }
            });
        result.whenComplete((value, error) -> {
            pending.remove(call);
            if (error != null) call.cancel(true);
        });
        return result;
    }

    @Override
    public void close() {
        if (closed.compareAndSet(false, true)) {
            for (var request : pending) request.cancel(true);
            pending.clear();
        }
    }

    private static final class LimitedBody implements HttpResponse.BodySubscriber<byte[]> {

        private final int maximum;
        private final CompletableFuture<byte[]> result = new CompletableFuture<>();
        private final ByteArrayOutputStream buffer = new ByteArrayOutputStream();
        private Flow.Subscription subscription;

        LimitedBody(int maximum) {
            this.maximum = maximum;
        }

        @Override
        public CompletionStage<byte[]> getBody() {
            return result;
        }

        @Override
        public void onSubscribe(Flow.Subscription subscription) {
            this.subscription = subscription;
            subscription.request(1);
        }

        @Override
        public void onNext(List<ByteBuffer> chunks) {
            for (var chunk : chunks) {
                if (chunk.remaining() > maximum - buffer.size()) {
                    subscription.cancel();
                    result.completeExceptionally(
                        new IllegalStateException("AgentVault HTTP response exceeds configured limit")
                    );
                    return;
                }
                byte[] bytes = new byte[chunk.remaining()];
                chunk.get(bytes);
                buffer.writeBytes(bytes);
            }
            subscription.request(1);
        }

        @Override
        public void onError(Throwable error) {
            result.completeExceptionally(error);
        }

        @Override
        public void onComplete() {
            result.complete(buffer.toByteArray());
        }
    }
}
