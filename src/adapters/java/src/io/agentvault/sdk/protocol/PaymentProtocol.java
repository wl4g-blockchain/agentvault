package io.agentvault.sdk.protocol;

import com.fasterxml.jackson.databind.JsonNode;

import io.agentvault.sdk.model.Base.HttpCredential;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Base.PaymentCredential;
import io.agentvault.sdk.model.Payee.SettledIncomingPayment;

import java.util.List;
import java.util.Map;

/** HTTP/MCP wire mapping only; keys, policy, and settlement remain in AgentVault. */
public interface PaymentProtocol {
    record HttpResponse(Map<String, String> headers, String contentType, byte[] body) {
        public HttpResponse {
            headers = Map.copyOf(headers);
            body = body == null ? new byte[0] : body.clone();
        }

        @Override
        public byte[] body() {
            return body.clone();
        }
    }

    record MerchantEvidence(String headerName, String headerValue) {}

    record McpMetadata(String key, JsonNode value) {
        public McpMetadata {
            if (key == null || key.isBlank() || value == null) {
                throw new IllegalArgumentException("MCP payment metadata is incomplete");
            }
        }
    }

    /** Exactly one of result or error is populated. */
    record McpPaymentRequired(JsonNode result, JsonNode error) {
        public McpPaymentRequired {
            if ((result == null) == (error == null)) {
                throw new IllegalArgumentException(
                        "MCP payment response must contain exactly one result or error");
            }
        }

        public static McpPaymentRequired result(JsonNode value) {
            return new McpPaymentRequired(value, null);
        }

        public static McpPaymentRequired error(JsonNode value) {
            return new McpPaymentRequired(null, value);
        }
    }

    String name();

    default List<String> credentialHeaderNames() {
        return List.of();
    }

    default List<String> merchantEvidenceHeaderNames() {
        return List.of();
    }

    HttpCredential credential(PaymentCredential credential);

    MerchantEvidence merchantEvidence(Map<String, String> headers);

    JsonNode presentedCredential(Map<String, String> headers, int limit);

    HttpResponse challenge(PaymentChallenge challenge);

    HttpResponse receipt(SettledIncomingPayment payment);

    default String mcpRequestMetadataKey() {
        throw new IllegalArgumentException("payment protocol does not support MCP: " + name());
    }

    default String mcpRootRequestMetadataKey() {
        return null;
    }

    default String mcpReceiptMetadataKey() {
        throw new IllegalArgumentException(
                "payment protocol does not support MCP receipts: " + name());
    }

    default HttpResponse mcpChallenge(JsonNode message, int limit) {
        return null;
    }

    default McpMetadata mcpCredential(PaymentCredential credential) {
        throw new IllegalArgumentException(
                "payment protocol does not support MCP credential delivery: " + name());
    }

    default JsonNode presentedMcpCredential(JsonNode meta, int limit) {
        return null;
    }

    default McpPaymentRequired mcpPaymentRequired(PaymentChallenge challenge) {
        throw new IllegalArgumentException(
                "payment protocol does not support MCP challenges: " + name());
    }

    default McpPaymentRequired mcpPaymentRejected(PaymentChallenge challenge, String message) {
        throw new IllegalArgumentException(
                "payment protocol does not support MCP verification failures: " + name());
    }

    default McpMetadata mcpReceipt(PaymentChallenge challenge, SettledIncomingPayment payment) {
        throw new IllegalArgumentException(
                "payment protocol does not support MCP receipts: " + name());
    }

    default MerchantEvidence mcpMerchantEvidence(JsonNode meta, int limit) {
        return null;
    }

    default String a2aExtension() {
        return "";
    }

    default HttpResponse a2aChallenge(JsonNode message, int limit) {
        return null;
    }

    default JsonNode presentedA2aCredential(JsonNode message, int limit) {
        return null;
    }

    default void a2aStripCredential(JsonNode message) {}

    default void a2aApplyCredential(JsonNode message, PaymentCredential credential) {
        throw new IllegalArgumentException("A2A unsupported");
    }

    default McpPaymentRequired a2aPaymentRequired(
            PaymentChallenge challenge, io.a2a.spec.Task task, boolean rejected) {
        throw new IllegalArgumentException("A2A unsupported");
    }

    default MerchantEvidence a2aEvidence(JsonNode message, int limit) {
        return null;
    }

    default void a2aInjectReceipt(
            JsonNode message, PaymentChallenge challenge, SettledIncomingPayment payment) {
        throw new IllegalArgumentException("A2A unsupported");
    }

    default String header(Map<String, String> headers, String wanted) {
        if (headers == null) return null;
        return headers.entrySet().stream()
                .filter(entry -> entry.getKey().equalsIgnoreCase(wanted))
                .map(Map.Entry::getValue)
                .findFirst()
                .orElse(null);
    }
}
