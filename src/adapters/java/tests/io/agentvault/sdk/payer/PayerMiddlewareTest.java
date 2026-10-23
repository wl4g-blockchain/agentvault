package io.agentvault.sdk.payer;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.stripe.mpp.Challenge;
import com.stripe.mpp.Credential;
import com.stripe.mpp.Receipt;
import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.transport.VaultTransport;
import io.agentvault.sdk.model.Payer.PayerHttpRequest;
import io.agentvault.sdk.model.Payer.PayerHttpResponse;
import io.agentvault.sdk.model.Payer.PayerRequestContext;
import io.agentvault.sdk.model.Payer.AuthorizedPayment;
import io.agentvault.sdk.model.Base.HttpCredential;
import io.agentvault.sdk.model.Base.PaymentCredential;
import io.agentvault.sdk.model.Base.PaymentIntent;
import java.util.ArrayList;
import java.util.Base64;
import java.util.List;
import java.util.Map;
import java.util.concurrent.atomic.AtomicInteger;
import java.time.Instant;
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

    @Test void forwardsNativeChallengeHeadersToAgentVault() {
        for (var challenge : Map.of(
                "payment-required", "eyJ4NDAyVmVyc2lvbiI6Mn0=",
                "www-authenticate", "Payment id=\"charge\", method=\"tempo\"").entrySet()) {
            var captured = new java.util.concurrent.atomic.AtomicReference<Object>();
            VaultTransport transport = new VaultTransport() {
                @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
                @Override public <T> T post(String path, Object body, Class<T> type) {
                    captured.set(body);
                    throw new IllegalStateException("vault called");
                }
            };
            var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario-1"));
            assertThrows(IllegalStateException.class, () -> core.authorize(
                    new PayerHttpRequest("POST", "https://merchant.test/mcp", "https://merchant.test",
                            Map.of(), new byte[0], false),
                    new PayerHttpResponse(402, Map.of(challenge.getKey(), challenge.getValue()), "{}".getBytes()),
                    new PayerRequestContext("run-1")));
            assertEquals(challenge.getValue(), new ObjectMapper().valueToTree(captured.get())
                    .path("challenge").path("headers").path(challenge.getKey()).asText());
        }
    }

    @Test void bindsMcpChallengesToJsonRpcVersionAndRequestId() throws Exception {
        var calls = new AtomicInteger();
        VaultTransport transport = new VaultTransport() {
            @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
            @Override public <T> T post(String path, Object body, Class<T> type) {
                calls.incrementAndGet();
                throw new IllegalStateException("Vault must not be called");
            }
        };
        var mapper = new ObjectMapper();
        var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario-1"));
        var request = new PayerHttpRequest(
                "POST", "https://merchant.test/mcp", "https://merchant.test", Map.of(),
                "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"tools/call\",\"params\":{\"name\":\"marketpulse.realtime\"}}"
                        .getBytes(java.nio.charset.StandardCharsets.UTF_8),
                false);
        for (String response : List.of(
                "{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{\"isError\":true}}",
                "{\"jsonrpc\":\"1.0\",\"id\":1,\"result\":{\"isError\":true}}")) {
            assertNull(core.authorizeMcp(
                    request, mapper.readTree(response), new PayerRequestContext("run-1")));
        }
        assertEquals(0, calls.get());
        assertEquals(true, PayerMiddleware.boundMcpResponse(
                mapper.readTree("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}"),
                mapper.readTree("1"), true));
        assertEquals(false, PayerMiddleware.boundMcpResponse(
                mapper.readTree("{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}"),
                mapper.readTree("1"), true));
    }

    @Test void reportsMppReceiptsUsingProtocolEvidence() throws Exception {
        var mapper = new ObjectMapper();
        var captured = new java.util.concurrent.atomic.AtomicReference<Object>();
        String intentJson = """
                {"id":"intent-1","scenario_id":"scenario-1","run_id":"run-1","wallet_id":"wallet",
                 "policy_id":"policy","direction":"outbound","protocol":"mpp",
                 "asset":{"network":"eip155:42431","asset":"USDC","decimals":6},
                 "recipient":"0x1","amount":"1000","settled_amount":"0",
                 "resource":"https://merchant.example/tool","challenge_hash":"challenge",
                 "request_hash":"request","status":"signed","reservation_status":"reserved",
                 "reserved_amount":"1000","created_at":1,"updated_at":1}
                """;
        PaymentIntent intent = mapper.readValue(intentJson, PaymentIntent.class);
        VaultTransport transport = new VaultTransport() {
            @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
            @Override public <T> T post(String path, Object body, Class<T> type) {
                captured.set(body);
                try {
                    return mapper.treeToValue(mapper.readTree("{\"intent\":" + intentJson
                            + ",\"accepted\":true,\"replayed\":false}"), type);
                } catch (com.fasterxml.jackson.core.JsonProcessingException error) {
                    throw new IllegalStateException(error);
                }
            }
        };
        var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario-1"));
        Challenge challenge = Challenge.create(
                "test-secret", "merchant.example", "tempo", "charge",
                Map.of("amount", "1000", "currency", "USDC"));
        String credential = withPaymentAuthorizationHeader(mapper, new Credential(challenge.toEcho(),
                Map.of("type", "hash", "hash", "0x1111"), null).toAuthorization());
        var payment = new AuthorizedPayment(intent, new PaymentCredential(
                "mpp", "challenge", "credential", mapper.createObjectNode(),
                new HttpCredential("https://merchant.example", "payment-authorization", credential)));
        assertEquals("payment-authorization", core.credentialHeader(
                payment, "https://merchant.example", Map.of("authorization", "Bearer app-token")).getKey());
        core.report(new PayerRequestContext("run-1"), payment,
                new PayerHttpResponse(200, Map.of("payment-receipt",
                        new Receipt("success", Instant.parse("2033-05-18T03:33:20Z"),
                                "0x1111", "tempo", "intent-1", null).toPaymentReceipt()),
                        new byte[0]));
        JsonNode wire = mapper.valueToTree(captured.get());
        assertEquals("mpp", wire.path("protocol").asText());
        assertEquals("payment-receipt", wire.path("evidence").path("header_name").asText());
        assertEquals(false, wire.has("payment_response"));
    }

    private static String withPaymentAuthorizationHeader(ObjectMapper mapper, String authorization)
            throws Exception {
        String token = authorization.substring("Payment ".length());
        token += "=".repeat((4 - token.length() % 4) % 4);
        var envelope = (com.fasterxml.jackson.databind.node.ObjectNode) mapper.readTree(
                Base64.getUrlDecoder().decode(token));
        ((com.fasterxml.jackson.databind.node.ObjectNode) envelope.path("challenge"))
                .put("header", "Payment-Authorization");
        return "Payment " + Base64.getUrlEncoder().withoutPadding()
                .encodeToString(mapper.writeValueAsBytes(envelope));
    }
}
