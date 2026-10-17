package io.agentvault.sdk.payer;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.model.Base.VaultTransport;
import io.agentvault.sdk.model.Payer.PayerHttpRequest;
import io.agentvault.sdk.model.Payer.PayerHttpResponse;
import io.agentvault.sdk.model.Payer.PayerRequestContext;
import java.util.ArrayList;
import java.util.List;
import java.util.Map;
import java.util.concurrent.atomic.AtomicInteger;
import org.junit.jupiter.api.Test;

final class PayerMiddlewareTest {
    @Test void retriesTheSameImmutableAuthorizationWhileApprovalIsPending() throws Exception {
        var mapper = new ObjectMapper();
        List<JsonNode> responses = List.of(
                mapper.readTree("{\"authorization\":{\"outcome\":\"requires_approval\",\"intent_id\":\"intent-1\"},\"intent\":{\"id\":\"intent-1\"}}"),
                mapper.readTree("{\"authorization\":{\"outcome\":\"authorized\",\"intent_id\":\"intent-1\"},"
                        + "\"intent\":{\"id\":\"intent-1\",\"scenario_id\":\"scenario-1\",\"run_id\":\"run-1\","
                        + "\"wallet_id\":\"wallet-1\",\"policy_id\":\"policy-1\",\"direction\":\"outbound\",\"protocol\":\"x402\","
                        + "\"asset\":{\"network\":\"eip155:1\",\"asset\":\"USDC\",\"decimals\":6},"
                        + "\"amount\":\"100\",\"recipient\":\"recipient-1\","
                        + "\"resource\":\"https://merchant.test/mcp\",\"challenge_hash\":\"challenge-1\","
                        + "\"request_hash\":\"request-1\"},"
                        + "\"credential\":{\"protocol\":\"x402\",\"challenge_hash\":\"challenge-1\","
                        + "\"credential_hash\":\"credential-1\",\"payload\":{}}}"));
        var calls = new ArrayList<Object>();
        var index = new AtomicInteger();
        VaultTransport transport = new VaultTransport() {
            @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
            @Override public <T> T post(String path, Object body, Class<T> type) {
                calls.add(body);
                try {
                    return mapper.treeToValue(responses.get(index.getAndIncrement()), type);
                } catch (com.fasterxml.jackson.core.JsonProcessingException error) {
                    throw new IllegalStateException(error);
                }
            }
        };
        var core = new PayerMiddleware(transport,
                new PayerConfig("scenario-1",
                        java.time.Duration.ofMillis(200), java.time.Duration.ofMillis(1), 1, 1024));
        var payment = core.authorize(
                new PayerHttpRequest("POST", "https://merchant.test/mcp", "https://merchant.test",
                        Map.of(), new byte[0], false),
                new PayerHttpResponse(402, Map.of(), "{}".getBytes(java.nio.charset.StandardCharsets.UTF_8)),
                new PayerRequestContext("run-1"));
        assertEquals("intent-1", payment.intent().id());
        assertEquals(2, calls.size());
        assertSame(calls.get(0), calls.get(1));
        JsonNode wire = mapper.valueToTree(calls.get(0));
        assertEquals("run-1", wire.path("run_id").asText());
        var fields = new java.util.TreeSet<String>();
        wire.fieldNames().forEachRemaining(fields::add);
        assertEquals(java.util.Set.of("scenario_id", "run_id", "challenge"), fields);
    }

    @Test void forwardsCapturedProtocolHintWithoutLocalProjection() {
        var called = new java.util.concurrent.atomic.AtomicBoolean();
        var captured = new java.util.concurrent.atomic.AtomicReference<Object>();
        VaultTransport transport = new VaultTransport() {
            @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
            @Override public <T> T post(String path, Object body, Class<T> type) {
                called.set(true);
                captured.set(body);
                throw new IllegalStateException("vault called");
            }
        };
        var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario-1"));
        assertThrows(IllegalStateException.class, () -> core.authorize(
                new PayerHttpRequest("POST", "https://merchant.test/mcp", "https://merchant.test",
                        Map.of(), new byte[0], false),
                new PayerHttpResponse(402, Map.of("payment-protocol", "mpp"), "{}".getBytes()),
                new PayerRequestContext("run-1")));
        assertEquals(true, called.get());
        assertEquals(
                "mpp",
                new ObjectMapper().valueToTree(captured.get()).path("challenge").path("protocol_hint").asText());
    }
}
