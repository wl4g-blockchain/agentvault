package io.agentvault.sdk.payee;

import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertThrows;
import com.fasterxml.jackson.databind.ObjectMapper;
import io.agentvault.sdk.config.Config.PayeeConfig;
import io.agentvault.sdk.model.Base.AssetIdentity;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Base.VaultTransport;
import io.agentvault.sdk.model.Payee.PayeeOperation;
import io.agentvault.sdk.model.Payee.PaymentRequired;
import java.util.ArrayList;
import java.util.List;
import org.junit.jupiter.api.Test;

final class PayeeMiddlewareTest {
    @Test void createsAChallengeWhenNoCredentialIsPresent() {
        VaultTransport transport = new VaultTransport() {
            private final ObjectMapper mapper = new ObjectMapper();
            @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
            @Override public <T> T post(String path, Object body, String key, Class<T> type) {
                return type.cast(new PaymentChallenge(
                        "scenario-1", "offer-1", "mcp://marketpulse/realtime", "x402",
                        new AssetIdentity("eip155:1", "USDC", null, 6), "100", "recipient-1",
                        "nonce-1", "challenge-1", 1, mapper.createObjectNode()));
            }
        };
        var core = new PayeeMiddleware(transport, PayeeConfig.defaults());
        assertInstanceOf(PaymentRequired.class, core.protect(new PayeeOperation(
                "scenario-1", "run-1", "run-1.realtime", "offer-1", "mcp://marketpulse/realtime",
                "https://marketpulse.test", "x402", "request-hash", null)));
    }

    @Test void rejectsInvalidPaymentBeforeSettlement() throws Exception {
        var mapper = new ObjectMapper();
        var paths = new ArrayList<String>();
        VaultTransport transport = new VaultTransport() {
            @Override public <T> T get(String path, Class<T> type) { throw new UnsupportedOperationException(); }
            @Override public <T> T post(String path, Object body, String key, Class<T> type) {
                paths.add(path);
                if (path.equals("/api/v1/payee/challenges")) {
                    return type.cast(new PaymentChallenge(
                            "scenario", "offer", "mcp://marketpulse/realtime", "x402",
                            new AssetIdentity("eip155:31337", "USDC", null, 6), "1000", "0x1",
                            "nonce", "challenge", 1, mapper.createObjectNode().set(
                                    "payment_required", mapper.createObjectNode())));
                }
                if (path.equals("/api/v1/payee/verify")) {
                    try {
                        return mapper.readValue("{\"intent\":{\"id\":\"intent\","
                                + "\"scenario_id\":\"scenario\",\"run_id\":\"run\",\"offer_id\":\"offer\","
                                + "\"direction\":\"inbound\",\"protocol\":\"x402\","
                                + "\"asset\":{\"network\":\"eip155:31337\",\"asset\":\"USDC\",\"decimals\":6},"
                                + "\"recipient\":\"0x1\",\"amount\":\"1000\",\"settled_amount\":\"0\","
                                + "\"resource\":\"mcp://marketpulse/realtime\",\"challenge_hash\":\"challenge\","
                                + "\"idempotency_key\":\"execution.challenge\",\"request_hash\":\"request-hash\","
                                + "\"status\":\"pending_settlement\",\"reservation_status\":\"reserved\","
                                + "\"reserved_amount\":\"1000\",\"created_at\":1,\"updated_at\":1},"
                                + "\"valid\":false}", type);
                    } catch (com.fasterxml.jackson.core.JsonProcessingException error) {
                        throw new IllegalStateException(error);
                    }
                }
                throw new AssertionError("unexpected Vault call after invalid verification: " + path);
            }
        };
        var operation = new PayeeOperation(
                "scenario", "run", "execution", "offer", "mcp://marketpulse/realtime",
                "https://marketpulse.example", "x402", "request-hash",
                mapper.createObjectNode().put("signature", "present"));
        assertThrows(SecurityException.class,
                () -> new PayeeMiddleware(transport, PayeeConfig.defaults()).protect(operation));
        assertEquals(List.of("/api/v1/payee/challenges", "/api/v1/payee/verify"), paths);
    }
}
