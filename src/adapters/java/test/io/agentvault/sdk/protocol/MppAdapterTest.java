package io.agentvault.sdk.protocol;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.stripe.mpp.Challenge;
import com.stripe.mpp.Credential;

import io.agentvault.sdk.model.Base.AssetIdentity;
import io.agentvault.sdk.model.Base.HttpCredential;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Base.PaymentCredential;

import org.junit.jupiter.api.Test;

import java.nio.charset.StandardCharsets;
import java.util.Base64;
import java.util.List;
import java.util.Map;

final class MppAdapterTest {
    @Test
    void preservesBearerAndEmitsOnlyNativeMppChallengeHeaders() throws Exception {
        Challenge nativeChallenge =
                Challenge.create(
                        "test-secret",
                        "merchant.example",
                        "tempo",
                        "charge",
                        Map.of("amount", "1000", "currency", "USDC"));
        ObjectMapper json = new ObjectMapper();
        ObjectNode payload =
                json.createObjectNode()
                        .put(
                                "www_authenticate",
                                nativeChallenge.toWwwAuthenticate()
                                        + ", header=\"Payment-Authorization\"");
        PaymentChallenge challenge =
                new PaymentChallenge(
                        "scenario",
                        "price",
                        "https://merchant.example/tool",
                        "mpp",
                        new AssetIdentity("eip155:42431", "USDC", null, 6),
                        "1000",
                        "0x1111111111111111111111111111111111111111",
                        "nonce",
                        "challenge",
                        2_000_000_000L,
                        payload);
        var adapter = new MppAdapter();
        var response = adapter.challenge(challenge);
        assertTrue(response.headers().containsKey("www-authenticate"));
        assertFalse(response.headers().containsKey("payment-required"));
        String credential =
                withPaymentAuthorizationHeader(
                        json,
                        new Credential(
                                        nativeChallenge.toEcho(),
                                        Map.of("type", "hash", "hash", "0x1111"),
                                        null)
                                .toAuthorization());
        assertEquals(
                credential,
                adapter.presentedCredential(
                                Map.of(
                                        "authorization",
                                        "Bearer app-token",
                                        "payment-authorization",
                                        credential),
                                4096)
                        .path("authorization")
                        .asText());
        assertThrows(
                IllegalArgumentException.class,
                () -> adapter.presentedCredential(Map.of("authorization", credential), 4096));
        PaymentCredential vaultCredential =
                new PaymentCredential(
                        "mpp",
                        "challenge",
                        "credential",
                        json.createObjectNode(),
                        new HttpCredential(
                                "https://merchant.example", "Payment-Authorization", credential));
        var metadata = adapter.mcpCredential(vaultCredential);
        assertEquals(
                "Payment-Authorization",
                metadata.value().path("challenge").path("header").asText());
        ObjectNode meta = json.createObjectNode();
        meta.set(MppAdapter.MCP_CREDENTIAL, metadata.value());
        String projected =
                adapter.presentedMcpCredential(meta, 4096).path("authorization").asText();
        assertEquals("Payment-Authorization", credentialHeader(json, projected));
        PaymentCredential wrongDelivery =
                new PaymentCredential(
                        "mpp",
                        "challenge",
                        "credential",
                        json.createObjectNode(),
                        new HttpCredential(
                                "https://merchant.example", "Authorization", credential));
        assertThrows(IllegalArgumentException.class, () -> adapter.credential(wrongDelivery));
    }

    @Test
    void realTempoOpaqueIsPreservedAcrossMcpAndA2aChallengesAndCredentialEchoes() throws Exception {
        ObjectMapper json = new ObjectMapper();
        ObjectNode wire = realTempoChallenge(json);
        String originalOpaque = wire.path("opaque").asText();
        MppAdapter adapter = new MppAdapter();
        ObjectNode response = json.createObjectNode().put("jsonrpc", "2.0").put("id", 1);
        response.set("error", adapter.mcpPaymentRequired(vaultChallenge(json, wire)).error());
        assertEquals(wire, response.at("/error/data/challenges/0"));
        var mcp = adapter.mcpChallenge(response, 65536);
        var a2a = adapter.a2aChallenge(response, 65536);
        assertEquals(mcp.headers(), a2a.headers());
        Challenge nativeChallenge =
                Challenge.fromWwwAuthenticate(mcp.headers().get("www-authenticate")).get(0);
        assertEquals(originalOpaque, nativeChallenge.opaqueRaw());
        assertEquals("price-rust-tempo-mcp", nativeChallenge.opaque().get("price_id"));
        assertEquals(wire.path("id").asText(), nativeChallenge.id());
        assertEquals(wire.path("expires").asText(), nativeChallenge.expires());
        assertTrue(
                mcp.headers().get("www-authenticate").endsWith("header=\"Payment-Authorization\""));

        // The payload is a unit fixture; only the real public challenge carrier is under test.
        String authorization =
                withPaymentAuthorizationHeader(
                        json,
                        new Credential(
                                        nativeChallenge.toEcho(),
                                        Map.of("type", "hash", "hash", "0x1111"),
                                        null)
                                .toAuthorization());
        PaymentCredential payment =
                new PaymentCredential(
                        "mpp",
                        "challenge",
                        "credential",
                        json.createObjectNode(),
                        new HttpCredential(
                                "http://marketpulse-rust-tempo.e2e-agentvault-mock-payee.svc.cluster.local:8081",
                                "Payment-Authorization",
                                authorization));
        ObjectNode meta = json.createObjectNode();
        meta.set(MppAdapter.MCP_CREDENTIAL, adapter.mcpCredential(payment).value());
        assertEquals(
                originalOpaque, meta.at("/org.paymentauth~1credential/challenge/opaque").asText());
        String replayed =
                adapter.presentedMcpCredential(meta, 65536).path("authorization").asText();
        assertEquals(
                originalOpaque, Credential.fromAuthorization(replayed).challenge().opaqueRaw());
        assertEquals(nativeChallenge.id(), Credential.fromAuthorization(replayed).challenge().id());
        assertEquals("Payment-Authorization", credentialHeader(json, replayed));
        ObjectNode a2aRequest =
                (ObjectNode)
                        json.readTree(
                                """
{"jsonrpc":"2.0","id":1,"method":"message/send","params":{"message":{
 "kind":"message","messageId":"request-1","role":"user","parts":[{"kind":"text","text":"news"}]}}}
""");
        a2aRequest.set("_meta", meta);
        String a2aAuthorization =
                adapter.presentedA2aCredential(a2aRequest, 65536).path("authorization").asText();
        assertEquals(
                originalOpaque,
                Credential.fromAuthorization(a2aAuthorization).challenge().opaqueRaw());
    }

    @Test
    void opaqueWireBytesAreNotChangedByJsonMapReserialization() throws Exception {
        ObjectMapper json = new ObjectMapper();
        ObjectNode wire = realTempoChallenge(json);
        String opaque =
                Base64.getUrlEncoder()
                        .withoutPadding()
                        .encodeToString(
                                "{\"z\":\"last\", \"a\":\"first\"}"
                                        .getBytes(StandardCharsets.UTF_8));
        wire.put("opaque", opaque);
        MppAdapter adapter = new MppAdapter();
        ObjectNode response = json.createObjectNode().put("jsonrpc", "2.0").put("id", 1);
        response.set("error", adapter.mcpPaymentRequired(vaultChallenge(json, wire)).error());
        var header = adapter.mcpChallenge(response, 65536).headers().get("www-authenticate");
        Challenge parsed = Challenge.fromWwwAuthenticate(header).get(0);
        assertEquals(Map.of("a", "first", "z", "last"), parsed.opaque());
        assertEquals(opaque, parsed.opaqueRaw());
        assertEquals(opaque, parsed.toEcho().opaqueRaw());
    }

    @Test
    void malformedOrObjectOpaqueDoesNotReachTheVault() throws Exception {
        ObjectMapper json = new ObjectMapper();
        MppAdapter adapter = new MppAdapter();
        for (JsonNode opaque :
                List.of(
                        json.createObjectNode().put("legacy", "object"),
                        json.getNodeFactory().numberNode(7),
                        json.getNodeFactory().textNode("not-base64!"),
                        json.getNodeFactory()
                                .textNode(
                                        Base64.getUrlEncoder()
                                                .withoutPadding()
                                                .encodeToString(
                                                        "[]".getBytes(StandardCharsets.UTF_8))),
                        json.getNodeFactory()
                                .textNode(
                                        Base64.getUrlEncoder()
                                                .withoutPadding()
                                                .encodeToString(
                                                        "null".getBytes(StandardCharsets.UTF_8))),
                        json.getNodeFactory().textNode("abc\r\ninjected"))) {
            ObjectNode wire = realTempoChallenge(json);
            wire.set("opaque", opaque);
            assertThrows(
                    IllegalArgumentException.class,
                    () -> adapter.mcpPaymentRequired(vaultChallenge(json, wire)));
        }
    }

    private static PaymentChallenge vaultChallenge(ObjectMapper json, ObjectNode wire) {
        ObjectNode payload = json.createObjectNode();
        payload.set("mcp_challenge", wire);
        return new PaymentChallenge(
                "scenario-marketpulse-feed-tempo",
                "price-rust-tempo-mcp",
                wire.at("/request/_mppx_scope/resource").asText(),
                "mpp",
                new AssetIdentity(
                        "eip155:1337", "pathUSD", wire.at("/request/currency").asText(), 6),
                "1000000",
                wire.at("/request/recipient").asText(),
                "nonce",
                "challenge",
                2_000_000_000L,
                payload);
    }

    private static ObjectNode realTempoChallenge(ObjectMapper json) throws Exception {
        return (ObjectNode)
                json.readTree(
                        """
{
  "id": "1him3PQmFieK-4wyOlTCcOkgMvjpEM29eBTzEo4dGnk",
  "realm": "agentvault-manual",
  "method": "tempo",
  "intent": "charge",
  "request": {
    "_mppx_scope": {
      "resource": "http://marketpulse-rust-tempo.e2e-agentvault-mock-payee.svc.cluster.local:8081/mcp"
    },
    "amount": "1000000",
    "currency": "0x20c0000000000000000000000000000000000000",
    "externalId": "price-rust-tempo-mcp",
    "methodDetails": {
      "chainId": 1337
    },
    "recipient": "0x052b91ad9732d1bce0ddae15a4545e5c65d02443"
  },
  "expires": "2026-10-26T16:30:26Z",
  "opaque": "eyJjaGFsbGVuZ2VfaWQiOiJkMGNmNTQ1Ny1lM2FlLTRkMjEtYjJkMi04ZmQ2NzgxOWE3OGMiLCJwcmljZV9pZCI6InByaWNlLXJ1c3QtdGVtcG8tbWNwIiwicmVzb3VyY2UiOiJodHRwOi8vbWFya2V0cHVsc2UtcnVzdC10ZW1wby5lMmUtYWdlbnR2YXVsdC1tb2NrLXBheWVlLnN2Yy5jbHVzdGVyLmxvY2FsOjgwODEvbWNwIiwic2NlbmFyaW9faWQiOiJzY2VuYXJpby1tYXJrZXRwdWxzZS1mZWVkLXRlbXBvIn0",
  "header": "Payment-Authorization"
}
""");
    }

    private static String withPaymentAuthorizationHeader(ObjectMapper json, String authorization)
            throws Exception {
        String token = authorization.substring("Payment ".length());
        ObjectNode envelope = (ObjectNode) json.readTree(Base64.getUrlDecoder().decode(pad(token)));
        ((ObjectNode) envelope.path("challenge")).put("header", "Payment-Authorization");
        return "Payment "
                + Base64.getUrlEncoder()
                        .withoutPadding()
                        .encodeToString(json.writeValueAsBytes(envelope));
    }

    private static String credentialHeader(ObjectMapper json, String authorization)
            throws Exception {
        String token = authorization.substring("Payment ".length());
        return json.readTree(Base64.getUrlDecoder().decode(pad(token)))
                .path("challenge")
                .path("header")
                .asText();
    }

    private static String pad(String value) {
        return value + "=".repeat((4 - value.length() % 4) % 4);
    }
}
