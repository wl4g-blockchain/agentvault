package io.agentvault.sdk.payer;

import static org.junit.jupiter.api.Assertions.*;

import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.transport.VaultTransport;

import org.junit.jupiter.api.Test;

import java.nio.charset.StandardCharsets;
import java.util.Map;

final class SpringPayerMessagesTest {
    @Test
    void preservesApplicationMetadataAndRejectsExistingCredentials() throws Exception {
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
                new SpringPayerMessages(
                        new PayerMiddleware(transport, PayerConfig.defaults("scenario")));
        byte[] request =
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"news\",\"_meta\":{\"application\":true}}}"
                        .getBytes(StandardCharsets.UTF_8);
        var call = messages.parse("POST", Map.of("content-type", "application/json"), request);
        assertNotNull(call);
        byte[] paid =
                messages.credential(
                        call, "x402/payment", SpringPayerMessages.JSON.createObjectNode());
        var wire = SpringPayerMessages.JSON.readTree(paid);
        assertTrue(wire.path("params").path("_meta").path("application").asBoolean());
        var existing = messages.parse("POST", Map.of("content-type", "application/json"), paid);
        assertThrows(
                IllegalArgumentException.class,
                () ->
                        messages.credential(
                                existing,
                                "x402/payment",
                                SpringPayerMessages.JSON.createObjectNode()));
        assertNull(messages.parse("GET", Map.of("content-type", "application/json"), request));
    }
}
