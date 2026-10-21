package io.agentvault.sdk.protocol;

import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertNull;
import static org.junit.jupiter.api.Assertions.assertTrue;

import com.fasterxml.jackson.databind.ObjectMapper;
import io.agentvault.sdk.model.Base.PaymentChallenge;
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
}
