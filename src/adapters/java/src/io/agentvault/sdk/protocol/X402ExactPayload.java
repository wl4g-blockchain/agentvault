package io.agentvault.sdk.protocol;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;

import java.util.Base64;

/** Chain-specific projection of the official x402 v2 {@code PaymentPayload.payload}. */
sealed interface X402ExactPayload permits X402ExactPayload.Eip3009, X402ExactPayload.SolanaExact {

    static X402ExactPayload parse(
            ObjectMapper json, JsonNode accepted, JsonNode payload, String label) {
        if (!"exact".equals(accepted.path("scheme").asText())) {
            throw new IllegalArgumentException(label + " uses an unsupported x402 scheme");
        }
        String network = accepted.path("network").asText();
        try {
            if (network.startsWith("eip155:")) {
                requireExactFields(payload, "signature", "authorization");
                JsonNode authorization = payload.path("authorization");
                requireExactFields(
                        authorization, "from", "to", "value", "validAfter", "validBefore", "nonce");
                requireText(payload.path("signature"), label, "signature");
                for (String field :
                        new String[] {
                            "from", "to", "value", "validAfter", "validBefore", "nonce"
                        }) {
                    requireText(authorization.path(field), label, "authorization." + field);
                }
                Eip3009 value = json.treeToValue(payload, Eip3009.class);
                JsonNode extra = accepted.path("extra");
                if (!isText(extra.path("name")) || !isText(extra.path("version"))) {
                    throw new IllegalArgumentException(label + " has no EIP-3009 domain metadata");
                }
                return value;
            }
            if (network.startsWith("solana:")) {
                requireExactFields(payload, "transaction");
                requireText(payload.path("transaction"), label, "transaction");
                SolanaExact value = json.treeToValue(payload, SolanaExact.class);
                byte[] transaction = Base64.getDecoder().decode(value.transaction());
                if (transaction.length == 0
                        || !Base64.getEncoder()
                                .encodeToString(transaction)
                                .equals(value.transaction())) {
                    throw new IllegalArgumentException(
                            label + " has no canonical base64 Solana transaction");
                }
                if (!isText(accepted.path("extra").path("feePayer"))) {
                    throw new IllegalArgumentException(
                            label + " has no Solana facilitator fee payer");
                }
                return value;
            }
        } catch (JsonProcessingException | IllegalArgumentException error) {
            if (error instanceof IllegalArgumentException argument) throw argument;
            throw new IllegalArgumentException(
                    label + " has invalid chain-specific payload fields", error);
        }
        throw new IllegalArgumentException(label + " uses an unsupported x402 network");
    }

    private static boolean blank(String value) {
        return value == null || value.isBlank();
    }

    private static boolean isText(JsonNode value) {
        return value.isTextual() && !blank(value.textValue());
    }

    private static void requireText(JsonNode value, String label, String field) {
        if (!isText(value)) {
            throw new IllegalArgumentException(
                    label + " has invalid chain-specific field " + field);
        }
    }

    private static void requireExactFields(JsonNode value, String... expected) {
        if (!value.isObject() || value.size() != expected.length) {
            throw new IllegalArgumentException("x402 chain-specific payload has unexpected fields");
        }
        for (String field : expected) {
            if (!value.has(field)) {
                throw new IllegalArgumentException("x402 chain-specific payload omitted " + field);
            }
        }
    }

    record Eip3009(String signature, Authorization authorization) implements X402ExactPayload {}

    record Authorization(
            String from,
            String to,
            String value,
            String validAfter,
            String validBefore,
            String nonce) {}

    record SolanaExact(String transaction) implements X402ExactPayload {}
}
