package io.agentvault.sdk.transport;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import io.agentvault.sdk.config.Config.MqttConfig;
import io.agentvault.sdk.model.Base.ApiException;
import io.agentvault.sdk.model.Base.VaultTransport;
import java.security.SecureRandom;
import java.util.Base64;
import java.util.Set;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ConcurrentHashMap;
import java.util.concurrent.TimeUnit;
import org.eclipse.paho.client.mqttv3.IMqttDeliveryToken;
import org.eclipse.paho.client.mqttv3.IMqttToken;
import org.eclipse.paho.client.mqttv3.MqttCallback;
import org.eclipse.paho.client.mqttv3.MqttClient;
import org.eclipse.paho.client.mqttv3.MqttConnectOptions;
import org.eclipse.paho.client.mqttv3.MqttException;
import org.eclipse.paho.client.mqttv3.MqttMessage;
import org.eclipse.paho.client.mqttv3.persist.MemoryPersistence;

public final class MqttVaultTransport implements VaultTransport, AutoCloseable {
    private static final ObjectMapper JSON = new ObjectMapper();
    private static final SecureRandom RANDOM = new SecureRandom();
    private static final String AUTHORIZE_PATH = "/api/v1/payments/authorize";
    private static final String PAYMENTS_PATH = "/api/v1/payments/";
    private static final Set<String> RESPONSE_FIELDS = Set.of(
            "type", "version", "correlationId", "status", "headers", "body");

    private record Pending(String responseType, CompletableFuture<JsonNode> future) {}
    private record Operation(String command, String messageType, String responseType) {}

    private final MqttConfig config;
    private final String prefix;
    private final ConcurrentHashMap<String, Pending> pending = new ConcurrentHashMap<>();
    private MqttClient client;

    public MqttVaultTransport(MqttConfig config) {
        this.config = java.util.Objects.requireNonNull(config, "config");
        this.prefix = "agentvault/v1/clients/" + config.vaultClientId();
    }

    public synchronized void connect() {
        if (client != null && client.isConnected()) return;
        try {
            String host = config.host().contains(":") ? "[" + config.host() + "]" : config.host();
            String broker = (config.tls() ? "ssl://" : "tcp://") + host + ":" + config.port();
            client = new MqttClient(
                    broker,
                    config.connectionPrefix() + "-" + randomId().substring(0, 12),
                    new MemoryPersistence());
            client.setCallback(new MqttCallback() {
                @Override public void connectionLost(Throwable cause) {}
                @Override public void deliveryComplete(IMqttDeliveryToken token) {}
                @Override public void messageArrived(String topic, MqttMessage message) {
                    onMessage(topic, message);
                }
            });
            MqttConnectOptions options = new MqttConnectOptions();
            options.setMqttVersion(MqttConnectOptions.MQTT_VERSION_3_1_1);
            options.setCleanSession(true);
            options.setAutomaticReconnect(false);
            options.setConnectionTimeout((int) Math.max(1, config.timeout().toSeconds()));
            options.setUserName(config.username());
            options.setPassword(config.password().toCharArray());
            client.connect(options);
            IMqttToken subscription = client.subscribeWithResponse(
                    new String[]{prefix + "/results/+"}, new int[]{1});
            subscription.waitForCompletion(config.timeout().toMillis());
            if (!subscriptionGranted(subscription.getGrantedQos(), 1)) {
                throw new IllegalStateException("AgentVault MQTT subscriptions were rejected or downgraded");
            }
        } catch (MqttException error) {
            close();
            throw new IllegalStateException("MQTT connect/subscribe failed: " + error.getReasonCode(), error);
        } catch (RuntimeException error) {
            close();
            throw error;
        }
    }

    @Override public <T> T get(String path, Class<T> type) {
        throw new IllegalArgumentException("MQTT transport does not support this GET operation");
    }

    @Override public <T> T post(String path, Object body, Class<T> type) {
        Operation operation = operation(path);
        if (body == null) throw new IllegalArgumentException("MQTT payment commands require a request body");
        if (client == null || !client.isConnected()) {
            throw new IllegalStateException("MQTT transport is not connected");
        }
        if (pending.size() >= 256) throw new IllegalStateException("too many pending MQTT payment requests");
        String correlation = randomId();
        CompletableFuture<JsonNode> future = new CompletableFuture<>();
        Pending waiting = new Pending(operation.responseType(), future);
        if (pending.putIfAbsent(correlation, waiting) != null) {
            throw new IllegalStateException("MQTT correlation collision");
        }
        try {
            byte[] bodyBytes = JSON.writeValueAsBytes(body);
            ObjectNode headers = JSON.createObjectNode();
            headers.put("accept", "application/json");
            headers.put("content-type", "application/json");
            ObjectNode request = JSON.createObjectNode();
            request.put("method", "POST");
            request.put("path", path);
            request.set("headers", headers);
            request.put("body", Base64.getUrlEncoder().withoutPadding().encodeToString(bodyBytes));
            ObjectNode envelope = JSON.createObjectNode();
            envelope.put("type", operation.messageType());
            envelope.put("version", 1);
            envelope.put("correlationId", correlation);
            envelope.set("request", request);
            byte[] raw = JSON.writeValueAsBytes(envelope);
            if (raw.length > config.maxResponseBytes()) {
                throw new IllegalArgumentException("MQTT payment request exceeds configured limit");
            }
            MqttMessage message = new MqttMessage(raw);
            message.setQos(1);
            message.setRetained(false);
            client.publish(prefix + "/commands/" + operation.command(), message);
            JsonNode response = future.get(config.timeout().toMillis(), TimeUnit.MILLISECONDS);
            byte[] decoded;
            try {
                decoded = Base64.getUrlDecoder().decode(response.path("body").asText());
            } catch (IllegalArgumentException error) {
                throw new IllegalStateException("invalid AgentVault MQTT response body", error);
            }
            if (decoded.length > config.maxResponseBytes()
                    || !Base64.getUrlEncoder().withoutPadding().encodeToString(decoded)
                        .equals(response.path("body").asText())) {
                throw new IllegalStateException("invalid or oversized AgentVault MQTT response body");
            }
            JsonNode value = JSON.readTree(decoded);
            int status = response.path("status").asInt();
            if (status < 200 || status >= 300) {
                JsonNode error = value.path("error");
                throw new ApiException(status, error.path("code").asText("HTTP_ERROR"),
                        error.path("message").asText("AgentVault returned HTTP " + status));
            }
            return JSON.treeToValue(value, type);
        } catch (java.util.concurrent.TimeoutException error) {
            throw new IllegalStateException("AgentVault MQTT request timed out", error);
        } catch (InterruptedException error) {
            Thread.currentThread().interrupt();
            throw new IllegalStateException("AgentVault MQTT request interrupted", error);
        } catch (java.io.IOException | MqttException error) {
            throw new IllegalStateException("AgentVault MQTT request failed", error);
        } catch (java.util.concurrent.ExecutionException error) {
            throw new IllegalStateException("AgentVault MQTT request failed", error.getCause());
        } finally {
            pending.remove(correlation, waiting);
        }
    }

    private void onMessage(String topic, MqttMessage message) {
        if (message.getPayload().length > config.maxResponseBytes()) return;
        try {
            JsonNode value = JSON.readTree(message.getPayload());
            String correlation = value.path("correlationId").asText("");
            Pending waiting = pending.get(correlation);
            int status = value.path("status").asInt(-1);
            String responseType = value.path("type").asText("");
            boolean responseTypeMatches = status >= 400
                    ? "payment.error".equals(responseType)
                    : waiting != null && waiting.responseType().equals(responseType);
            if (waiting == null || !responseTypeMatches || value.path("version").asInt() != 1
                    || !correlation.matches("[0-9a-f]{32}")
                    || !topic.equals(prefix + "/results/" + correlation)
                    || !value.path("status").canConvertToInt() || status < 100 || status > 599
                    || !value.path("headers").isObject() || !value.path("body").isTextual()
                    || value.size() != RESPONSE_FIELDS.size() || !RESPONSE_FIELDS.stream().allMatch(value::has)
                    || !headersAreStrings(value.path("headers"))) {
                return;
            }
            waiting.future().complete(value);
        } catch (Exception ignored) {
            // Malformed broker input fails closed.
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
        for (int qos : grants) {
            if (qos != 1) return false;
        }
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

    private static String randomId() {
        byte[] bytes = new byte[16];
        RANDOM.nextBytes(bytes);
        return java.util.HexFormat.of().formatHex(bytes);
    }

    @Override public synchronized void close() {
        if (client != null) {
            try {
                if (client.isConnected()) client.disconnect();
                client.close();
            } catch (MqttException ignored) {
                // Best-effort close cannot make a completed payment fail.
            }
            client = null;
        }
        pending.forEach((key, waiting) -> waiting.future().completeExceptionally(
                new IllegalStateException("MQTT transport closed")));
        pending.clear();
    }

    @Override public String toString() {
        return "MqttVaultTransport[config=" + config + ", state=<redacted>]";
    }
}
