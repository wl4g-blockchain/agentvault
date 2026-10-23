package io.agentvault.sdk.protocol;

import static org.junit.jupiter.api.Assertions.assertEquals;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import io.agentvault.sdk.model.Base.HttpCredential;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Base.PaymentCredential;
import java.util.Base64;
import java.util.List;
import java.util.Map;
import org.junit.jupiter.api.Test;

final class X402AdapterTest {
    @Test void emitsOnlyNativeX402ChallengeHeaders() throws Exception {
        PaymentChallenge challenge = new ObjectMapper().readValue("""
                {"scenario_id":"scenario","offer_id":"offer","resource":"https://merchant.example/tool",
                 "protocol":"x402","asset":{"network":"eip155:42431","asset":"USDC","decimals":6},
                 "amount":"1000","recipient":"0x1111111111111111111111111111111111111111",
                 "nonce":"nonce","challenge_hash":"challenge","expires_at":2000000000,
                 "payload":{"payment_required":{"x402Version":2,
                   "resource":{"url":"https://merchant.example/tool","mimeType":"application/json"},
                   "accepts":[{"scheme":"exact","network":"eip155:42431","amount":"1000",
                     "payTo":"0x1111111111111111111111111111111111111111","maxTimeoutSeconds":60,
                     "asset":"0x2222222222222222222222222222222222222222"}]}}}
                """, PaymentChallenge.class);
        var response = new X402Adapter().challenge(challenge);
        assertTrue(response.headers().containsKey("payment-required"));
        assertFalse(response.headers().containsKey("www-authenticate"));
    }

    @Test void ignoresUnrelatedMcpToolErrors() throws Exception {
        var message = new ObjectMapper().readTree("""
                {"jsonrpc":"2.0","id":1,"result":{"isError":true,
                 "structuredContent":{"error":"permission denied"},
                 "content":[{"type":"text","text":"permission denied"}]}}
                """);
        assertNull(new X402Adapter().mcpChallenge(message, 64 * 1024));
    }

    @Test void preservesChainSpecificV2PayloadsAndRejectsCrossovers() throws Exception {
        ObjectMapper json = new ObjectMapper();
        var stream = getClass().getClassLoader().getResourceAsStream("x402_v2_chain_payloads.json");
        assertNotNull(stream, "shared x402 fixture");
        JsonNode fixture;
        try (stream) {
            fixture = json.readTree(stream);
        }
        JsonNode evm = fixture.at("/cases/evm_eip3009/payment_payload");
        JsonNode solana = fixture.at("/cases/solana_spl_exact/payment_payload");
        X402Adapter adapter = new X402Adapter();

        for (JsonNode payload : List.of(evm, solana)) {
            String header = Base64.getEncoder().encodeToString(json.writeValueAsBytes(payload));
            HttpCredential delivery = new HttpCredential(
                    "https://merchant.example", "PAYMENT-SIGNATURE", header);
            PaymentCredential credential = new PaymentCredential(
                    "x402", "challenge", "credential", payload, delivery);

            assertEquals(delivery, adapter.credential(credential));
            var metadata = adapter.mcpCredential(credential);
            assertEquals("x402/payment", metadata.key());
            assertEquals(payload, metadata.value());
            assertEquals(payload, adapter.presentedCredential(
                    Map.of("PAYMENT-SIGNATURE", header), 64 * 1024));
            assertEquals(payload, adapter.presentedMcpCredential(
                    json.createObjectNode().set(metadata.key(), metadata.value()), 64 * 1024));
        }

        var crossover = solana.deepCopy();
        ((com.fasterxml.jackson.databind.node.ObjectNode) crossover)
                .set("accepted", evm.path("accepted"));
        assertThrows(IllegalArgumentException.class, () -> adapter.presentedMcpCredential(
                json.createObjectNode().set("x402/payment", crossover), 64 * 1024));

        var expanded = evm.deepCopy();
        ((com.fasterxml.jackson.databind.node.ObjectNode) expanded.path("payload"))
                .put("unexpected", true);
        assertThrows(IllegalArgumentException.class, () -> adapter.presentedMcpCredential(
                json.createObjectNode().set("x402/payment", expanded), 64 * 1024));

        var wrongType = evm.deepCopy();
        ((com.fasterxml.jackson.databind.node.ObjectNode)
                wrongType.at("/payload/authorization")).put("validAfter", 0);
        assertThrows(IllegalArgumentException.class, () -> adapter.presentedMcpCredential(
                json.createObjectNode().set("x402/payment", wrongType), 64 * 1024));
    }
}
