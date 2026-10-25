package io.agentvault.sdk.protocol;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.stripe.mpp.Challenge;
import com.stripe.mpp.Credential;
import io.agentvault.sdk.model.Base.AssetIdentity;
import io.agentvault.sdk.model.Base.HttpCredential;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Base.PaymentCredential;
import java.util.Base64;
import java.util.Map;
import org.junit.jupiter.api.Test;

final class MppAdapterTest {
    @Test void preservesBearerAndEmitsOnlyNativeMppChallengeHeaders() throws Exception {
        Challenge nativeChallenge = Challenge.create(
                "test-secret", "merchant.example", "tempo", "charge",
                Map.of("amount", "1000", "currency", "USDC"));
        ObjectMapper json = new ObjectMapper();
        ObjectNode payload = json.createObjectNode()
                .put("www_authenticate", nativeChallenge.toWwwAuthenticate()
                        + ", header=\"Payment-Authorization\"");
        PaymentChallenge challenge = new PaymentChallenge(
                "scenario", "offer", "https://merchant.example/tool", "mpp",
                new AssetIdentity("eip155:42431", "USDC", null, 6), "1000",
                "0x1111111111111111111111111111111111111111", "nonce", "challenge",
                2_000_000_000L, payload);
        var adapter = new MppAdapter();
        var response = adapter.challenge(challenge);
        assertTrue(response.headers().containsKey("www-authenticate"));
        assertFalse(response.headers().containsKey("payment-required"));
        String credential = withPaymentAuthorizationHeader(json, new Credential(nativeChallenge.toEcho(),
                Map.of("type", "hash", "hash", "0x1111"), null).toAuthorization());
        assertEquals(credential, adapter.presentedCredential(Map.of(
                "authorization", "Bearer app-token",
                "payment-authorization", credential), 4096).path("authorization").asText());
        assertThrows(IllegalArgumentException.class, () ->
                adapter.presentedCredential(Map.of("authorization", credential), 4096));
        PaymentCredential vaultCredential = new PaymentCredential(
                "mpp", "challenge", "credential", json.createObjectNode(),
                new HttpCredential(
                        "https://merchant.example", "Payment-Authorization", credential));
        var metadata = adapter.mcpCredential(vaultCredential);
        assertEquals("Payment-Authorization",
                metadata.value().path("challenge").path("header").asText());
        ObjectNode meta = json.createObjectNode();
        meta.set(MppAdapter.MCP_CREDENTIAL, metadata.value());
        String projected = adapter.presentedMcpCredential(meta, 4096)
                .path("authorization").asText();
        assertEquals("Payment-Authorization", credentialHeader(json, projected));
        PaymentCredential wrongDelivery = new PaymentCredential(
                "mpp", "challenge", "credential", json.createObjectNode(),
                new HttpCredential("https://merchant.example", "Authorization", credential));
        assertThrows(IllegalArgumentException.class, () -> adapter.credential(wrongDelivery));
    }

    private static String withPaymentAuthorizationHeader(ObjectMapper json, String authorization)
            throws Exception {
        String token = authorization.substring("Payment ".length());
        ObjectNode envelope = (ObjectNode) json.readTree(Base64.getUrlDecoder().decode(pad(token)));
        ((ObjectNode) envelope.path("challenge")).put("header", "Payment-Authorization");
        return "Payment " + Base64.getUrlEncoder().withoutPadding()
                .encodeToString(json.writeValueAsBytes(envelope));
    }

    private static String credentialHeader(ObjectMapper json, String authorization) throws Exception {
        String token = authorization.substring("Payment ".length());
        return json.readTree(Base64.getUrlDecoder().decode(pad(token)))
                .path("challenge").path("header").asText();
    }

    private static String pad(String value) {
        return value + "=".repeat((4 - value.length() % 4) % 4);
    }
}
