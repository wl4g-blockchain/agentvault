package io.agentvault.sdk.payer;

import static org.junit.jupiter.api.Assertions.*;

import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.transport.VaultTransport;

import org.junit.jupiter.api.Test;

import java.nio.charset.StandardCharsets;
import java.util.List;
import java.util.Map;

final class PayerMessagesTest {
    @Test
    void preservesBothApplicationMetadataScopesAndRejectsExistingCredentials() throws Exception {
        var transport =
                new VaultTransport() {
                    public <T> T get(String path, Class<T> type) {
                        throw new AssertionError("unexpected Vault call");
                    }

                    public <T> T post(String path, Object body, Class<T> type) {
                        throw new AssertionError("unexpected Vault call");
                    }
                };
        var messages =
                new PayerMessages(new PayerMiddleware(transport, PayerConfig.defaults("scenario")));
        byte[] request =
                ("{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\","
                     + "\"_meta\":{\"root/application\":true},\"params\":{\"name\":\"news\",\"_meta\":{\"application\":true}}}")
                        .getBytes(StandardCharsets.UTF_8);
        for (String key : List.of("x402/payment", "org.paymentauth/credential")) {
            var call = messages.parse("POST", Map.of("content-type", "application/json"), request);
            assertNotNull(call);
            byte[] paid = messages.credential(call, key, PayerMessages.JSON.createObjectNode());
            var wire = PayerMessages.JSON.readTree(paid);
            assertTrue(wire.path("params").path("_meta").path("application").asBoolean());
            assertTrue(wire.path("_meta").path("root/application").asBoolean());
            boolean root = key.equals("org.paymentauth/credential");
            assertEquals(root, wire.path("_meta").has(key));
            assertEquals(!root, wire.path("params").path("_meta").has(key));
            var existing = messages.parse("POST", Map.of("content-type", "application/json"), paid);
            assertThrows(
                    IllegalArgumentException.class,
                    () ->
                            messages.credential(
                                    existing, key, PayerMessages.JSON.createObjectNode()));
            for (String location : List.of("root", "params")) {
                var occupied = PayerMessages.JSON.readTree(request);
                var meta =
                        (com.fasterxml.jackson.databind.node.ObjectNode)
                                (location.equals("root")
                                        ? occupied.path("_meta")
                                        : occupied.path("params").path("_meta"));
                meta.putObject(key);
                var occupiedCall =
                        messages.parse(
                                "POST",
                                Map.of("content-type", "application/json"),
                                PayerMessages.JSON.writeValueAsBytes(occupied));
                assertThrows(
                        IllegalArgumentException.class,
                        () ->
                                messages.credential(
                                        occupiedCall, key, PayerMessages.JSON.createObjectNode()));
            }
        }
        assertNull(messages.parse("GET", Map.of("content-type", "application/json"), request));
    }
}
