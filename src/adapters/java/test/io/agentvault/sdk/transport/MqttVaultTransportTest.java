package io.agentvault.sdk.transport;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertTrue;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertArrayEquals;
import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.JsonNode;
import java.io.DataInputStream;
import java.io.ByteArrayOutputStream;
import java.net.ServerSocket;
import java.net.Socket;
import java.net.InetAddress;
import java.util.Map;
import java.util.LinkedHashMap;
import java.util.HashSet;
import java.util.ArrayList;
import java.util.Collections;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.ExecutionException;
import java.util.concurrent.TimeUnit;
import org.eclipse.paho.client.mqttv3.MqttMessage;
import org.eclipse.paho.client.mqttv3.internal.wire.MqttWireMessage;
import org.eclipse.paho.client.mqttv3.internal.wire.MqttPublish;
import org.eclipse.paho.client.mqttv3.internal.wire.MqttPubAck;
import io.agentvault.sdk.config.Config.MqttConfig;
import java.time.Duration;
import org.junit.jupiter.api.Test;
import org.junit.jupiter.api.Timeout;

final class MqttVaultTransportTest {
    @Test void rejectsTopicInjectionAndRedactsCredentials() {
        assertThrows(IllegalArgumentException.class, () -> new MqttConfig(
                "localhost", 1883, false, "user", "pass", "worker", "a/../b", Duration.ofSeconds(1), 1024));
        var config = new MqttConfig(
                "localhost", 1883, false, "secret-user", "secret-pass", "worker", "client-a",
                Duration.ofSeconds(1), 1024);
        assertFalse(config.toString().contains("secret-user"));
        assertFalse(config.toString().contains("secret-pass"));
    }

    @Test void requiresExactQosOneSubscriptionGrants() {
        assertTrue(MqttVaultTransport.subscriptionGranted(new int[]{1}, 1));
        assertFalse(MqttVaultTransport.subscriptionGranted(new int[]{128}, 1));
        assertFalse(MqttVaultTransport.subscriptionGranted(new int[]{1, 1}, 1));
    }

    @Test @Timeout(15) void reconnectReplaysExactEnvelopesAndPairsDuplicateReplies() throws Exception {
        try (var listener = new ServerSocket(0, 2, InetAddress.getLoopbackAddress());
             var transport = new MqttVaultTransport(config(listener.getLocalPort(), Duration.ofSeconds(8)))) {
            listener.setSoTimeout(10000);
            var peer = new Peer(listener);
            var peerResult = CompletableFuture.runAsync(() -> {
                try { peer.run(); } catch (Exception error) { throw new RuntimeException(error); }
            });
            var first = transport.postAsync("/api/v1/payee/challenges", Map.of("runId", "one"), JsonNode.class);
            var second = transport.postAsync("/api/v1/payee/challenges", Map.of("runId", "two"), JsonNode.class);
            peerResult.exceptionally(error -> {
                first.completeExceptionally(error);
                second.completeExceptionally(error);
                return null;
            });
            assertEquals("one", first.get(10, TimeUnit.SECONDS).path("runId").asText());
            assertEquals("two", second.get(10, TimeUnit.SECONDS).path("runId").asText());
            peerResult.get(2, TimeUnit.SECONDS);
        }
    }

    @Test @Timeout(5) void cancellationAndCloseStayBoundedWithoutBrokerAcknowledgements() throws Exception {
        try (var listener = new ServerSocket(0, 2, InetAddress.getLoopbackAddress());
             var transport = new MqttVaultTransport(config(listener.getLocalPort(), Duration.ofMillis(200)))) {
            var cancelled = transport.postAsync("/api/v1/payee/challenges", Map.of("runId", "cancel"), JsonNode.class);
            assertTrue(cancelled.cancel(false));
            var timed = transport.postAsync("/api/v1/payee/challenges", Map.of("runId", "timeout"), JsonNode.class);
            assertInstanceOf(java.util.concurrent.TimeoutException.class,
                    assertThrows(ExecutionException.class, () -> timed.get(2, TimeUnit.SECONDS)).getCause());
            var closed = transport.postAsync("/api/v1/payee/challenges", Map.of("runId", "close"), JsonNode.class);
            transport.close();
            transport.close();
            assertInstanceOf(IllegalStateException.class,
                    assertThrows(ExecutionException.class, () -> closed.get(1, TimeUnit.SECONDS)).getCause());
        }
    }

    @Test @Timeout(5) void connectionFailureCallbackCanCloseBeforeRetry() throws Exception {
        int port;
        try (var listener = new ServerSocket(0, 1, InetAddress.getLoopbackAddress())) {
            port = listener.getLocalPort();
        }
        try (var transport = new MqttVaultTransport(config(port, Duration.ofSeconds(2)))) {
            var closed = new CompletableFuture<Void>();
            transport.connectAsync().whenComplete((ignored, error) -> {
                transport.close();
                closed.complete(null);
            });
            closed.get(3, TimeUnit.SECONDS);
            assertTrue(transport.postAsync("/api/v1/payee/challenges", Map.of(), JsonNode.class)
                    .isCompletedExceptionally());
        }
    }

    private static MqttConfig config(int port, Duration timeout) {
        return new MqttConfig("127.0.0.1", port, false, "user", "pass", "worker", "client", timeout, 65536);
    }

    private static final class Peer {
        private static final ObjectMapper JSON = new ObjectMapper();
        private final ServerSocket listener;
        private byte[] lastPayload;
        private Peer(ServerSocket listener) { this.listener = listener; }

        private MqttWireMessage read(Socket socket) throws Exception {
            var input = new DataInputStream(socket.getInputStream());
            int header = input.readUnsignedByte();
            long length = MqttWireMessage.readMBI(input).getValue();
            assertTrue(length <= 65536);
            byte[] body = new byte[(int)length];
            input.readFully(body);
            lastPayload = body;
            var bytes = new ByteArrayOutputStream();
            bytes.write(header);
            bytes.write(MqttWireMessage.encodeMBI(length));
            bytes.write(body);
            return MqttWireMessage.createWireMessage(bytes.toByteArray());
        }

        private void send(Socket socket, MqttWireMessage packet) throws Exception {
            socket.getOutputStream().write(packet.getHeader());
            socket.getOutputStream().write(packet.getPayload());
            socket.getOutputStream().flush();
        }

        private Map<String, byte[]> commands(Socket socket) throws Exception {
            socket.setSoTimeout(8000);
            assertEquals(MqttWireMessage.MESSAGE_TYPE_CONNECT, read(socket).getType());
            socket.getOutputStream().write(new byte[]{0x20, 2, 0, 0});
            Map<String, byte[]> commands = new LinkedHashMap<>();
            boolean subscribed = false;
            while (commands.size() < 2 || !subscribed) {
                MqttWireMessage packet = read(socket);
                if (packet.getType() == MqttWireMessage.MESSAGE_TYPE_SUBSCRIBE) {
                    assertEquals(1, lastPayload[lastPayload.length - 1]);
                    int id = packet.getMessageId();
                    socket.getOutputStream().write(new byte[]{(byte)0x90, 3, (byte)(id >> 8), (byte)id, 1});
                    subscribed = true;
                } else {
                    MqttPublish publish = assertInstanceOf(MqttPublish.class, packet);
                    assertEquals(1, publish.getMessage().getQos());
                    assertFalse(publish.getMessage().isRetained());
                    JsonNode request = JSON.readTree(publish.getPayload());
                    String span = request.path("requestSpanId").asText();
                    assertEquals(span, request.path("request").path("headers").path("traceparent").asText().split("-")[2]);
                    commands.put(span, publish.getPayload());
                    send(socket, new MqttPubAck(publish));
                }
            }
            return commands;
        }

        private void run() throws Exception {
            Map<String, byte[]> original;
            try (var first = listener.accept()) { original = commands(first); }
            try (var second = listener.accept()) {
                var replay = commands(second);
                assertEquals(original.keySet(), replay.keySet());
                for (var span : original.keySet()) assertArrayEquals(original.get(span), replay.get(span));
                var entries = new ArrayList<>(original.entrySet());
                Collections.reverse(entries);
                int id = 1;
                for (var entry : entries) {
                    var response = JSON.createObjectNode();
                    response.put("type", "payment.challenge_created").put("version", 1)
                            .put("requestSpanId", entry.getKey()).put("status", 200);
                    response.putObject("headers");
                    response.put("body", JSON.readTree(entry.getValue()).path("request").path("body").asText());
                    var message = new MqttMessage(JSON.writeValueAsBytes(response));
                    message.setQos(1);
                    var publish = new MqttPublish("agentvault/v1/clients/client/results/" + entry.getKey(), message);
                    publish.setMessageId(id++);
                    send(second, publish);
                    publish.setDuplicate(true);
                    send(second, publish);
                }
                var acknowledgements = new HashSet<Integer>();
                while (acknowledgements.size() < 2) {
                    var packet = read(second);
                    if (packet instanceof MqttPubAck) acknowledgements.add(packet.getMessageId());
                    else send(second, new MqttPubAck(assertInstanceOf(MqttPublish.class, packet)));
                }
            }
        }
    }
}
