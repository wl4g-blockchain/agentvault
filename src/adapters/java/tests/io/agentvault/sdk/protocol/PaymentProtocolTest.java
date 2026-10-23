package io.agentvault.sdk.protocol;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotSame;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.stripe.mpp.Challenge;
import com.stripe.mpp.Credential;
import com.stripe.mpp.Receipt;
import io.agentvault.sdk.model.Base.HttpCredential;
import io.agentvault.sdk.model.Base.AssetIdentity;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Base.PaymentCredential;
import io.agentvault.sdk.model.Payee.SettledIncomingPayment;
import java.util.List;
import java.util.Map;
import java.util.HashSet;
import java.util.Set;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.charset.StandardCharsets;
import java.time.Instant;
import java.util.Base64;
import org.junit.jupiter.api.Test;

final class PaymentProtocolTest {
    private static final ObjectMapper JSON = new ObjectMapper();

    @Test void factoryInstancesAreIsolatedAndRejectDuplicates() {
        var first = new PaymentProtocolFactory();
        first.register("test", MppAdapter::new);
        assertTrue(assertThrows(IllegalArgumentException.class,
                () -> first.create(List.of("test"))).getMessage().contains("mismatched"));

        var second = new PaymentProtocolFactory();
        assertTrue(assertThrows(IllegalArgumentException.class,
                () -> second.create(List.of("test"))).getMessage().contains("unavailable"));
        assertThrows(IllegalArgumentException.class, () -> second.create(List.of("x402", "x402")));
        assertNotSame(second.create(List.of("x402")).get("x402"),
                second.create(List.of("x402")).get("x402"));
    }

    @Test void mppPreservesBearerAndRejectsTwoPaymentCredentials() {
        PaymentProtocol mpp = new PaymentProtocolFactory().create(List.of("mpp")).get("mpp");
        Challenge challenge = mppChallenge();
        String credential = mppCredential(challenge);
        assertEquals(credential, mpp.presentedCredential(Map.of(
                "authorization", "Bearer app-token",
                "payment-authorization", credential), 4096).path("authorization").asText());
        assertThrows(IllegalArgumentException.class, () -> mpp.presentedCredential(Map.of(
                "authorization", credential,
                "payment-authorization", credential), 4096));
        assertEquals("payment-authorization", mpp.credential(new PaymentCredential(
                "mpp", "challenge", "credential", JSON.createObjectNode(),
                new HttpCredential("https://merchant.example", "payment-authorization", credential)))
                .headerName());
    }

    @Test void x402AndMppWireFormatsAreNeverInterchanged() throws Exception {
        ProtocolRegistry protocols = new PaymentProtocolFactory().create(List.of("x402", "mpp"));
        Challenge nativeChallenge = mppChallenge();
        ObjectNode payload = JSON.createObjectNode()
                .put("www_authenticate", nativeChallenge.toWwwAuthenticate());
        PaymentChallenge challenge = new PaymentChallenge(
                "scenario", "offer", "https://merchant.example/tool", "mpp",
                new AssetIdentity("eip155:42431", "USDC", null, 6), "1000",
                "0x1111111111111111111111111111111111111111", "nonce", "challenge",
                2_000_000_000L, payload);
        var challengeWire = protocols.get("mpp").challenge(challenge);
        assertTrue(challengeWire.headers().containsKey("www-authenticate"));
        assertFalse(challengeWire.headers().containsKey("payment-required"));

        SettledIncomingPayment payment = JSON.readValue("""
                {"intent":{"id":"intent","scenario_id":"scenario","run_id":"run","wallet_id":"wallet",
                 "policy_id":"policy","offer_id":"offer","direction":"inbound","protocol":"mpp",
                 "asset":{"network":"eip155:42431","asset":"USDC","decimals":6},
                 "recipient":"0x1111111111111111111111111111111111111111","amount":"1000",
                 "settled_amount":"1000","resource":"https://merchant.example/tool",
                 "challenge_hash":"challenge","request_hash":"request","status":"settled",
                 "reservation_status":"consumed","reserved_amount":"0","tx_hash":"0x1111",
                 "created_at":1,"updated_at":2,"settled_at":2},
                 "receipt":{"intent_id":"intent","scenario_id":"scenario","direction":"inbound",
                 "protocol":"mpp","asset":{"network":"eip155:42431","asset":"USDC","decimals":6},
                 "recipient":"0x1111111111111111111111111111111111111111",
                 "payer":"0x2222222222222222222222222222222222222222","requested_amount":"1000",
                 "settled_amount":"1000","status":"settled","tx_hash":"0x1111",
                 "evidence_hash":"evidence","confirmed_at":2000000000}}
                """, SettledIncomingPayment.class);
        var receiptWire = protocols.get("mpp").receipt(payment);
        assertTrue(receiptWire.headers().containsKey("payment-receipt"));
        assertFalse(receiptWire.headers().containsKey("payment-response"));
    }
    @Test void sharedFixtureCoversBothTransportsProtocolsAndRoles() throws Exception {
        var fixture = JSON.readTree(Files.readString(Path.of("..", "fixtures", "payment_protocol_http.json")));
        assertEquals("agentvault.sdk.protocol-http.v1", fixture.path("schema").asText());
        var registry = new PaymentProtocolFactory().create(List.of("x402", "mpp"));
        var protocols = fixture.path("protocols");
        var combinedEvidence = new java.util.LinkedHashMap<String, String>();
        for (String name : List.of("x402", "mpp")) {
            String header = protocols.path(name).path("evidence_header").asText();
            String evidence = name.equals("x402")
                    ? Base64.getEncoder().encodeToString(("{\"success\":true,"
                            + "\"payer\":\"0x3333333333333333333333333333333333333333\","
                            + "\"transaction\":\"0x1111\",\"network\":\"eip155:42431\"}")
                            .getBytes(StandardCharsets.UTF_8))
                    : new Receipt("success", Instant.parse("2033-05-18T03:33:20Z"),
                            "0x1111", "tempo", "intent-1", null).toPaymentReceipt();
            assertEquals(header, registry.get(name).merchantEvidence(Map.of(header, evidence)).headerName());
            combinedEvidence.put(header, evidence);
        }
        assertThrows(IllegalArgumentException.class,
                () -> registry.merchantEvidence(combinedEvidence));
        assertEquals(List.of("payment-signature", "payment-authorization", "authorization"),
                registry.credentialHeaderNames());
        assertEquals(List.of("payment-response", "payment-receipt"),
                registry.merchantEvidenceHeaderNames());
        Set<String> expected = Set.of(
                "grpc/x402/payer", "grpc/x402/payee", "grpc/mpp/payer", "grpc/mpp/payee",
                "mqtt/x402/payer", "mqtt/x402/payee", "mqtt/mpp/payer", "mqtt/mpp/payee");
        var actual = new HashSet<String>();
        fixture.path("matrix").forEach(row -> actual.add(
                row.path("transport").asText() + "/"
                        + row.path("protocol").asText() + "/" + row.path("role").asText()));
        assertEquals(8, fixture.path("matrix").size());
        assertEquals(expected, actual);
    }

    @Test void sharedMcpFixtureUsesNativeProtocolModels() throws Exception {
        var fixture = JSON.readTree(Files.readString(
                Path.of("..", "fixtures", "payment_protocol_mcp.json")));
        assertEquals("agentvault.sdk.protocol-mcp.v1", fixture.path("schema").asText());
        var registry = new PaymentProtocolFactory().create(List.of("x402", "mpp"));
        ObjectNode combinedCredentials = JSON.createObjectNode();
        ObjectNode combinedReceipts = JSON.createObjectNode();
        for (String name : List.of("x402", "mpp")) {
            JsonNode contract = fixture.path("protocols").path(name);
            var challenge = registry.mcpChallenge(contract.path("challenge_response"), 64 * 1024);
            assertEquals(name, challenge.adapter().name());
            assertEquals(name, registry.presentedMcpCredential(
                    contract.path("credential_meta"), 64 * 1024).protocol());
            assertEquals(name, registry.mcpMerchantEvidence(
                    contract.path("receipt_meta"), 64 * 1024).protocol());
            combinedCredentials.setAll((ObjectNode) contract.path("credential_meta"));
            combinedReceipts.setAll((ObjectNode) contract.path("receipt_meta"));
        }
        assertThrows(IllegalArgumentException.class,
                () -> registry.presentedMcpCredential(combinedCredentials, 64 * 1024));
        assertThrows(IllegalArgumentException.class,
                () -> registry.mcpMerchantEvidence(combinedReceipts, 64 * 1024));
        assertEquals(Set.of("x402/payment", "org.paymentauth/credential"),
                Set.copyOf(registry.mcpRequestMetadataKeys()));
        assertEquals(List.of("org.paymentauth/credential"),
                registry.mcpRootRequestMetadataKeys());
        assertEquals(Set.of("x402/payment-response", "org.paymentauth/receipt"),
                Set.copyOf(registry.mcpReceiptMetadataKeys()));
        assertEquals(8, fixture.path("matrix").size());
    }

    @Test void mppMcpRejectsUnimplementedMethodAndIntent() throws Exception {
        var fixture = JSON.readTree(Files.readString(
                Path.of("..", "fixtures", "payment_protocol_mcp.json")));
        var adapter = new MppAdapter();
        for (String[] unsupported : List.of(
                new String[]{"stripe", "charge"},
                new String[]{"tempo", "session"})) {
            ObjectNode message = fixture.path("protocols").path("mpp")
                    .path("challenge_response").deepCopy();
            ObjectNode challenge = (ObjectNode) message.path("error").path("data")
                    .path("challenges").get(0);
            challenge.put("method", unsupported[0]);
            challenge.put("intent", unsupported[1]);
            assertThrows(IllegalArgumentException.class,
                    () -> adapter.mcpChallenge(message, 64 * 1024));
        }
    }

    private static Challenge mppChallenge() {
        return Challenge.create(
                "test-secret", "merchant.example", "tempo", "charge",
                Map.of("amount", "1000", "currency", "USDC"));
    }

    private static String mppCredential(Challenge challenge) {
        String authorization = new Credential(challenge.toEcho(),
                Map.of("type", "hash", "hash", "0x1111"), null).toAuthorization();
        try {
            String token = authorization.substring("Payment ".length());
            ObjectNode envelope = (ObjectNode) JSON.readTree(
                    Base64.getUrlDecoder().decode(padBase64(token)));
            ((ObjectNode) envelope.path("challenge")).put("header", "Payment-Authorization");
            return "Payment " + Base64.getUrlEncoder().withoutPadding()
                    .encodeToString(JSON.writeValueAsBytes(envelope));
        } catch (Exception error) {
            throw new AssertionError("cannot build MPP test credential", error);
        }
    }

    private static String padBase64(String value) {
        return value + "=".repeat((4 - value.length() % 4) % 4);
    }
}
