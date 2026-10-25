package io.agentvault.sdk.transport;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import io.agentvault.sdk.config.Config.MqttConfig;
import io.agentvault.sdk.transport.VaultTransport.ApiException;
import java.io.IOException;
import java.security.SecureRandom;
import java.util.ArrayList;
import java.util.Base64;
import java.util.Set;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionException;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.Executors;
import java.util.concurrent.ScheduledExecutorService;
import java.util.concurrent.ScheduledFuture;
import java.util.concurrent.ThreadFactory;
import java.util.concurrent.TimeUnit;
import java.util.concurrent.TimeoutException;
import org.eclipse.paho.client.mqttv3.IMqttActionListener;
import org.eclipse.paho.client.mqttv3.IMqttDeliveryToken;
import org.eclipse.paho.client.mqttv3.IMqttToken;
import org.eclipse.paho.client.mqttv3.MqttAsyncClient;
import org.eclipse.paho.client.mqttv3.MqttCallbackExtended;
import org.eclipse.paho.client.mqttv3.MqttConnectOptions;
import org.eclipse.paho.client.mqttv3.MqttException;
import org.eclipse.paho.client.mqttv3.MqttMessage;
import org.eclipse.paho.client.mqttv3.persist.MemoryPersistence;

/** Persistent QoS 1 MQTT transport with callback-based completion and replay. */
public final class MqttVaultTransport implements AsyncVaultTransport {
    private static final ObjectMapper JSON = new ObjectMapper();
    private static final SecureRandom RANDOM = new SecureRandom();
    private static final System.Logger LOG = System.getLogger(MqttVaultTransport.class.getName());
    private static final String AUTHORIZE_PATH = "/api/v1/payments/authorize";
    private static final String PAYMENTS_PATH = "/api/v1/payments/";
    private static final Set<String> RESPONSE_FIELDS = Set.of(
            "type", "version", "requestSpanId", "status", "headers", "body");

    private static final class Pending {
        private final String responseType;
        private final String commandTopic;
        private final byte[] payload;
        private final CompletableFuture<JsonNode> future;
        private volatile ScheduledFuture<?> timeout;

        private Pending(String responseType, String commandTopic, byte[] payload) {
            this.responseType = responseType;
            this.commandTopic = commandTopic;
            this.payload = payload.clone();
            this.future = new CompletableFuture<>();
        }
    }

    private record Operation(String command, String messageType, String responseType) {}

    private final MqttConfig config;
    private final String prefix;
    private final ConcurrentHashMap<String, Pending> pending = new ConcurrentHashMap<>();
    private final ScheduledExecutorService scheduler;
    private MqttAsyncClient client;
    private CompletableFuture<Void> readyFuture = new CompletableFuture<>();
    private boolean started;
    private boolean ready;
    private boolean closed;

    public MqttVaultTransport(MqttConfig config) {
        this.config = java.util.Objects.requireNonNull(config, "config");
        this.prefix = "agentvault/v1/clients/" + config.vaultClientId();
        ThreadFactory threads = task -> {
            Thread thread = new Thread(task, "agentvault-mqtt-timeout");
            thread.setDaemon(true);
            return thread;
        };
        this.scheduler = Executors.newSingleThreadScheduledExecutor(threads);
    }

    /** Starts the connection without blocking the calling thread. */
    public synchronized CompletableFuture<Void> connectAsync() {
        if (closed) return CompletableFuture.failedFuture(new IllegalStateException("MQTT transport closed"));
        if (ready) return CompletableFuture.completedFuture(null);
        if (!started) startConnection();
        return readyFuture.thenApply(ignored -> (Void) null)
                .orTimeout(config.timeout().toMillis(), TimeUnit.MILLISECONDS);
    }

    public void connect() {
        await(connectAsync(), "MQTT connect/subscribe failed");
    }

    private void startConnection() {
        started = true;
        if (readyFuture.isDone()) readyFuture = new CompletableFuture<>();
        try {
            String host = config.host().contains(":") ? "[" + config.host() + "]" : config.host();
            String broker = (config.tls() ? "ssl://" : "tcp://") + host + ":" + config.port();
            client = new MqttAsyncClient(
                    broker,
                    config.connectionPrefix() + "-" + randomHex(6),
                    new MemoryPersistence());
            client.setCallback(callback());
            client.connect(connectOptions(), null, new IMqttActionListener() {
                @Override public void onSuccess(IMqttToken token) {
                    // Subscription and replay are driven by connectComplete.
                }

                @Override public void onFailure(IMqttToken token, Throwable cause) {
                    failInitialConnection(cause);
                }
            });
        } catch (MqttException error) {
            failInitialConnection(error);
        }
    }

    private MqttConnectOptions connectOptions() {
        MqttConnectOptions options = new MqttConnectOptions();
        options.setMqttVersion(MqttConnectOptions.MQTT_VERSION_3_1_1);
        options.setCleanSession(true);
        options.setAutomaticReconnect(true);
        options.setConnectionTimeout((int) Math.max(1, config.timeout().toSeconds()));
        options.setUserName(config.username());
        options.setPassword(config.password().toCharArray());
        return options;
    }

    private MqttCallbackExtended callback() {
        return new MqttCallbackExtended() {
            @Override public void connectComplete(boolean reconnect, String serverUri) {
                subscribeAndReplay();
            }

            @Override public void connectionLost(Throwable cause) {
                synchronized (MqttVaultTransport.this) {
                    ready = false;
                    if (readyFuture.isDone()) readyFuture = new CompletableFuture<>();
                }
            }

            @Override public void messageArrived(String topic, MqttMessage message) {
                onMessage(topic, message);
            }

            @Override public void deliveryComplete(IMqttDeliveryToken token) {
                // Application completion is bound to the response envelope,
                // not merely the broker PUBACK.
            }
        };
    }

    private void subscribeAndReplay() {
        MqttAsyncClient current;
        synchronized (this) {
            ready = false;
            if (readyFuture.isCompletedExceptionally()) readyFuture = new CompletableFuture<>();
            current = client;
        }
        if (current == null) return;
        try {
            current.subscribe(prefix + "/results/+", 1, null, new IMqttActionListener() {
                @Override public void onSuccess(IMqttToken token) {
                    if (!subscriptionGranted(token.getGrantedQos(), 1)) {
                        failAll(new IllegalStateException(
                                "AgentVault MQTT subscriptions were rejected or downgraded"));
                        return;
                    }
                    synchronized (MqttVaultTransport.this) {
                        if (closed) return;
                        ready = true;
                        readyFuture.complete(null);
                    }
                    for (String requestSpanId : new ArrayList<>(pending.keySet())) {
                        publishPending(requestSpanId);
                    }
                }

                @Override public void onFailure(IMqttToken token, Throwable cause) {
                    failAll(new IllegalStateException("AgentVault MQTT result subscription failed", cause));
                }
            });
        } catch (MqttException error) {
            failAll(new IllegalStateException("AgentVault MQTT result subscription failed", error));
        }
    }

    private synchronized void failInitialConnection(Throwable cause) {
        started = false;
        ready = false;
        readyFuture.completeExceptionally(new IllegalStateException("MQTT connection failed", cause));
        closeClient();
        client = null;
    }

    @Override public <T> CompletableFuture<T> getAsync(String path, Class<T> type) {
        return CompletableFuture.failedFuture(
                new IllegalArgumentException("MQTT transport does not support this GET operation"));
    }

    @Override public <T> T get(String path, Class<T> type) {
        return await(getAsync(path, type), "AgentVault MQTT GET failed");
    }

    @Override public <T> CompletableFuture<T> postAsync(String path, Object body, Class<T> type) {
        Operation operation;
        try {
            operation = operation(path);
            if (body == null) throw new IllegalArgumentException("MQTT payment commands require a request body");
        } catch (RuntimeException error) {
            return CompletableFuture.failedFuture(error);
        }
        return connectAsync().thenCompose(ignored -> submit(operation, path, body))
                .thenApply(value -> convert(value, type));
    }

    @Override public <T> T post(String path, Object body, Class<T> type) {
        return await(postAsync(path, body, type), "AgentVault MQTT request failed");
    }

    private CompletableFuture<JsonNode> submit(Operation operation, String path, Object body) {
        if (pending.size() >= 256) {
            return CompletableFuture.failedFuture(new IllegalStateException("too many pending MQTT payment requests"));
        }
        try {
            String traceId = randomHex(16);
            String requestSpanId = randomHex(8);
            String traceparent = "00-" + traceId + "-" + requestSpanId + "-01";
            LOG.log(System.Logger.Level.INFO,
                    "agentvault.sdk.transport.mqtt.produced trace_id={0} span_id={1} parent_id=",
                    traceId, requestSpanId);
            byte[] raw = requestEnvelope(operation, path, body, requestSpanId, traceparent);
            Pending waiting = new Pending(
                    operation.responseType(), prefix + "/commands/" + operation.command(), raw);
            if (pending.putIfAbsent(requestSpanId, waiting) != null) {
                return CompletableFuture.failedFuture(new IllegalStateException("MQTT requestSpanId collision"));
            }
            waiting.timeout = scheduler.schedule(
                    () -> timeout(requestSpanId, waiting), config.timeout().toMillis(), TimeUnit.MILLISECONDS);
            publishPending(requestSpanId);
            return waiting.future;
        } catch (IOException error) {
            return CompletableFuture.failedFuture(new IllegalStateException("encode MQTT request", error));
        }
    }

    private byte[] requestEnvelope(
            Operation operation, String path, Object body, String requestSpanId, String traceparent) throws IOException {
        ObjectNode headers = JSON.createObjectNode();
        headers.put("accept", "application/json");
        headers.put("content-type", "application/json");
        headers.put("traceparent", traceparent);
        ObjectNode request = JSON.createObjectNode();
        request.put("method", "POST");
        request.put("path", path);
        request.set("headers", headers);
        request.put("body", Base64.getUrlEncoder().withoutPadding().encodeToString(JSON.writeValueAsBytes(body)));
        ObjectNode envelope = JSON.createObjectNode();
        envelope.put("type", operation.messageType());
        envelope.put("version", 1);
        envelope.put("requestSpanId", requestSpanId);
        envelope.set("request", request);
        byte[] raw = JSON.writeValueAsBytes(envelope);
        if (raw.length > config.maxResponseBytes()) {
            throw new IllegalArgumentException("MQTT payment request exceeds configured limit");
        }
        return raw;
    }

    private void publishPending(String requestSpanId) {
        Pending waiting = pending.get(requestSpanId);
        MqttAsyncClient current;
        synchronized (this) {
            current = ready && !closed ? client : null;
        }
        if (waiting == null || current == null) return;
        try {
            current.publish(waiting.commandTopic, waiting.payload, 1, false, null, new IMqttActionListener() {
                @Override public void onSuccess(IMqttToken token) {}

                @Override public void onFailure(IMqttToken token, Throwable cause) {
                    // Automatic reconnect restores the result subscription and
                    // republishes this same envelope. The request timeout is
                    // the only terminal failure for transient delivery loss.
                }
            });
        } catch (MqttException ignored) {
            // A concurrent disconnect is recovered by connectComplete.
        }
    }

    private void timeout(String requestSpanId, Pending waiting) {
        if (pending.remove(requestSpanId, waiting)) {
            waiting.future.completeExceptionally(new TimeoutException("AgentVault MQTT request timed out"));
        }
    }

    private void onMessage(String topic, MqttMessage message) {
        if (message.getPayload().length > config.maxResponseBytes()) return;
        try {
            JsonNode response = JSON.readTree(message.getPayload());
            String requestSpanId = response.path("requestSpanId").asText("");
            Pending waiting = pending.get(requestSpanId);
            int status = response.path("status").asInt(-1);
            String responseType = response.path("type").asText("");
            boolean responseTypeMatches = status >= 400
                    ? "payment.error".equals(responseType)
                    : waiting != null && waiting.responseType.equals(responseType);
            if (waiting == null || !responseTypeMatches || response.path("version").asInt() != 1
                    || !requestSpanId.matches("(?!0{16})[0-9a-f]{16}")
                    || !topic.equals(prefix + "/results/" + requestSpanId)
                    || !response.path("status").canConvertToInt() || status < 100 || status > 599
                    || !response.path("headers").isObject() || !response.path("body").isTextual()
                    || response.size() != RESPONSE_FIELDS.size() || !RESPONSE_FIELDS.stream().allMatch(response::has)
                    || !headersAreStrings(response.path("headers"))) {
                return;
            }
            if (!pending.remove(requestSpanId, waiting)) return;
            if (waiting.timeout != null) waiting.timeout.cancel(false);
            try {
                waiting.future.complete(responseBody(response, status));
            } catch (RuntimeException | IOException error) {
                waiting.future.completeExceptionally(error);
            }
        } catch (Exception ignored) {
            // Malformed broker input fails closed and leaves the bounded
            // request pending for a valid response or timeout.
        }
    }

    private JsonNode responseBody(JsonNode response, int status) throws IOException {
        byte[] decoded;
        try {
            decoded = Base64.getUrlDecoder().decode(response.path("body").asText());
        } catch (IllegalArgumentException error) {
            throw new IOException("invalid AgentVault MQTT response body", error);
        }
        if (decoded.length > config.maxResponseBytes()
                || !Base64.getUrlEncoder().withoutPadding().encodeToString(decoded)
                    .equals(response.path("body").asText())) {
            throw new IOException("invalid or oversized AgentVault MQTT response body");
        }
        JsonNode value = JSON.readTree(decoded);
        if (status < 200 || status >= 300) {
            JsonNode error = value.path("error");
            throw new ApiException(status, error.path("code").asText("TRANSPORT_ERROR"),
                    error.path("message").asText("AgentVault returned status " + status));
        }
        return value;
    }

    private static <T> T convert(JsonNode value, Class<T> type) {
        try {
            return JSON.treeToValue(value, type);
        } catch (IOException error) {
            throw new CompletionException(new IllegalStateException("decode AgentVault MQTT response", error));
        }
    }

    private static boolean headersAreStrings(JsonNode headers) {
        for (var property : headers.properties()) {
            if (!property.getValue().isTextual()) return false;
        }
        return true;
    }

    static boolean subscriptionGranted(int[] grants, int expectedCount) {
        if (grants == null || grants.length != expectedCount) return false;
        for (int qos : grants) if (qos != 1) return false;
        return true;
    }

    private static Operation operation(String path) {
        if (AUTHORIZE_PATH.equals(path)) {
            return new Operation("authorize", "payment.authorize", "payment.authorization_result");
        }
        if (path.startsWith(PAYMENTS_PATH) && path.endsWith("/outbound-response")) {
            String intentId = path.substring(PAYMENTS_PATH.length(), path.length() - "/outbound-response".length());
            if (intentId.matches("[A-Za-z0-9_.-]{1,128}")) {
                return new Operation("outbound-response", "payment.outbound_response",
                        "payment.outbound_response_result");
            }
        }
        return switch (path) {
            case "/api/v1/offers/resolve" ->
                    new Operation("resolve-offer", "payment.resolve_offer", "payment.offer_resolved");
            case "/api/v1/payee/challenges" ->
                    new Operation("create-challenge", "payment.create_challenge", "payment.challenge_created");
            case "/api/v1/payee/verify" ->
                    new Operation("verify-payment", "payment.verify", "payment.verification_result");
            case "/api/v1/payee/settle" ->
                    new Operation("settle-payment", "payment.settle", "payment.settlement_result");
            default -> throw new IllegalArgumentException("MQTT transport does not support this payment operation");
        };
    }

    private static String randomHex(int size) {
        byte[] bytes = new byte[size];
        String value;
        do {
            RANDOM.nextBytes(bytes);
            value = java.util.HexFormat.of().formatHex(bytes);
        } while (value.chars().allMatch(character -> character == '0'));
        return value;
    }

    private static <T> T await(CompletableFuture<T> future, String message) {
        try {
            return future.get();
        } catch (InterruptedException error) {
            Thread.currentThread().interrupt();
            throw new IllegalStateException(message, error);
        } catch (ExecutionException error) {
            Throwable cause = error.getCause();
            if (cause instanceof CompletionException completion && completion.getCause() != null) {
                cause = completion.getCause();
            }
            if (cause instanceof RuntimeException runtime) throw runtime;
            throw new IllegalStateException(message, cause);
        }
    }

    private void failAll(RuntimeException error) {
        for (var entry : pending.entrySet()) {
            Pending waiting = entry.getValue();
            if (pending.remove(entry.getKey(), waiting)) {
                if (waiting.timeout != null) waiting.timeout.cancel(false);
                waiting.future.completeExceptionally(error);
            }
        }
        synchronized (this) {
            ready = false;
            readyFuture.completeExceptionally(error);
        }
    }

    @Override public synchronized void close() {
        if (closed) return;
        closed = true;
        ready = false;
        failAll(new IllegalStateException("MQTT transport closed"));
        closeClient();
        client = null;
        scheduler.shutdownNow();
    }

    private void closeClient() {
        if (client == null) return;
        try {
            if (client.isConnected()) client.disconnect().waitForCompletion(config.timeout().toMillis());
            client.close();
        } catch (MqttException ignored) {
            // Best-effort close cannot change an already completed payment.
        }
    }

    @Override public String toString() {
        return "MqttVaultTransport[config=" + config + ", state=<redacted>]";
    }
}
