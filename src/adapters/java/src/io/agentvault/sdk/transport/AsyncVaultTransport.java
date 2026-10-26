package io.agentvault.sdk.transport;

import java.util.concurrent.CompletableFuture;

/** Native asynchronous request contract implemented by callback-based transports. */
public interface AsyncVaultTransport extends VaultTransport {
    <T> CompletableFuture<T> getAsync(String path, Class<T> type);
    <T> CompletableFuture<T> postAsync(String path, Object body, Class<T> type);
}
