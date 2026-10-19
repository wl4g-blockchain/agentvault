package io.agentvault.sdk.transport;

/** Transport-neutral AgentVault request contract implemented only by gRPC and MQTT. */
public interface VaultTransport extends AutoCloseable {
    <T> T get(String path, Class<T> type);
    <T> T post(String path, Object body, Class<T> type);

    @Override default void close() {}

    final class ApiException extends RuntimeException {
        private final int status;
        private final String code;

        public ApiException(int status, String code, String message) {
            super("AgentVault API error " + status + " (" + code + "): " + message);
            this.status = status;
            this.code = code;
        }

        public int status() { return status; }
        public String code() { return code; }
    }
}
