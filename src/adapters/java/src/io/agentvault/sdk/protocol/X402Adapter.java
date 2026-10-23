package io.agentvault.sdk.protocol;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import io.agentvault.sdk.model.Base.HttpCredential;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Base.PaymentCredential;
import io.agentvault.sdk.model.Payee.SettledIncomingPayment;
import java.nio.charset.StandardCharsets;
import java.util.Base64;
import java.util.Map;

/** x402 v2 wire mapping. Signing and settlement authority remain in AgentVault. */
public final class X402Adapter implements PaymentProtocol {
    public static final String MCP_PAYMENT = "x402/payment";
    public static final String MCP_PAYMENT_RESPONSE = "x402/payment-response";
    private static final ObjectMapper JSON = new ObjectMapper();

    @Override public String name() { return "x402"; }

    @Override public java.util.List<String> credentialHeaderNames() {
        return java.util.List.of("payment-signature");
    }

    @Override public java.util.List<String> merchantEvidenceHeaderNames() {
        return java.util.List.of("payment-response");
    }

    @Override public HttpCredential credential(PaymentCredential credential) {
        HttpCredential delivery = credential.http();
        if (!name().equals(credential.protocol()) || delivery == null
                || !"payment-signature".equalsIgnoreCase(delivery.headerName())) {
            throw new IllegalArgumentException("x402 credential has no PAYMENT-SIGNATURE delivery");
        }
        paymentPayload(decode(delivery.headerValue(), "PAYMENT-SIGNATURE"), "PAYMENT-SIGNATURE");
        return delivery;
    }

    @Override public MerchantEvidence merchantEvidence(Map<String, String> headers) {
        String value = header(headers, "payment-response");
        if (value == null || value.isBlank()) return null;
        settlement(decode(value, "PAYMENT-RESPONSE"), "PAYMENT-RESPONSE");
        return new MerchantEvidence("payment-response", value);
    }

    @Override public JsonNode presentedCredential(Map<String, String> headers, int limit) {
        String value = header(headers, "payment-signature");
        if (value == null || value.isBlank()) return null;
        boundedHeader(value, limit, "PAYMENT-SIGNATURE");
        return paymentPayload(decode(value, "PAYMENT-SIGNATURE"), "PAYMENT-SIGNATURE");
    }

    @Override public HttpResponse challenge(PaymentChallenge challenge) {
        JsonNode required = storedPaymentRequired(challenge);
        byte[] body = jsonBytes(required, "x402 challenge");
        return new HttpResponse(
                Map.of("payment-required", Base64.getEncoder().encodeToString(body)),
                "application/json", body);
    }

    @Override public HttpResponse receipt(SettledIncomingPayment payment) {
        JsonNode value = paymentSettlement(payment);
        return new HttpResponse(
                Map.of("payment-response", encode(value, "x402 receipt")), null, null);
    }

    @Override public String mcpRequestMetadataKey() { return MCP_PAYMENT; }

    @Override public String mcpReceiptMetadataKey() { return MCP_PAYMENT_RESPONSE; }

    @Override public HttpResponse mcpChallenge(JsonNode message, int limit) {
        boundedJson(message, limit, "MCP x402 message");
        if (message == null || !message.isObject() || !message.path("result").isObject()
                || !message.path("result").path("isError").asBoolean(false)) return null;
        JsonNode result = message.path("result");
        JsonNode structured = null;
        if (result.path("structuredContent").isObject()
                && (result.path("structuredContent").has("x402Version")
                    || result.path("structuredContent").has("accepts"))) {
            structured = paymentRequired(
                    result.get("structuredContent"), "MCP x402 structuredContent");
        }
        JsonNode text = null;
        if (result.path("content").isArray()) {
            for (JsonNode item : result.path("content")) {
                if (!"text".equals(item.path("type").asText()) || !item.path("text").isTextual()) continue;
                try {
                    text = paymentRequired(JSON.readTree(item.path("text").textValue()),
                            "MCP x402 content text");
                    break;
                } catch (JsonProcessingException | IllegalArgumentException ignored) {
                    // Non-payment text content is valid alongside a payment challenge.
                }
            }
        }
        if (structured == null && text == null) return null;
        if (structured != null && text != null && !structured.equals(text)) {
            throw new IllegalArgumentException("MCP x402 structuredContent conflicts with content text");
        }
        JsonNode required = structured != null ? structured : text;
        byte[] body = jsonBytes(required, "MCP x402 PaymentRequired");
        if (body.length > limit) {
            throw new IllegalArgumentException("MCP x402 PaymentRequired exceeds configured limit");
        }
        return new HttpResponse(
                Map.of("payment-required", Base64.getEncoder().encodeToString(body)),
                "application/json", body);
    }

    @Override public McpMetadata mcpCredential(PaymentCredential credential) {
        HttpCredential delivery = credential(credential);
        return new McpMetadata(MCP_PAYMENT,
                paymentPayload(decode(delivery.headerValue(), "PAYMENT-SIGNATURE"), "PAYMENT-SIGNATURE"));
    }

    @Override public JsonNode presentedMcpCredential(JsonNode meta, int limit) {
        requireObject(meta, "MCP params._meta");
        JsonNode value = meta.get(MCP_PAYMENT);
        if (value == null) return null;
        boundedJson(value, limit, "MCP x402 payment metadata");
        return paymentPayload(value, "MCP x402 payment metadata");
    }

    @Override public McpPaymentRequired mcpPaymentRequired(PaymentChallenge challenge) {
        return McpPaymentRequired.result(paymentResult(storedPaymentRequired(challenge)));
    }

    @Override public McpPaymentRequired mcpPaymentRejected(PaymentChallenge challenge, String message) {
        ObjectNode required = storedPaymentRequired(challenge).deepCopy();
        required.put("error", message);
        return McpPaymentRequired.result(paymentResult(required));
    }

    @Override public McpMetadata mcpReceipt(
            PaymentChallenge ignored, SettledIncomingPayment payment) {
        return new McpMetadata(MCP_PAYMENT_RESPONSE, paymentSettlement(payment));
    }

    @Override public MerchantEvidence mcpMerchantEvidence(JsonNode meta, int limit) {
        requireObject(meta, "MCP result._meta");
        JsonNode value = meta.get(MCP_PAYMENT_RESPONSE);
        if (value == null) return null;
        boundedJson(value, limit, "MCP x402 receipt metadata");
        JsonNode confirmed = settlement(value, "MCP x402 settlement metadata");
        return new MerchantEvidence("payment-response", encode(confirmed, "MCP x402 settlement metadata"));
    }

    private JsonNode storedPaymentRequired(PaymentChallenge challenge) {
        JsonNode required = challenge.payload() == null ? null : challenge.payload().get("payment_required");
        if (!name().equals(challenge.protocol()) || required == null) {
            throw new IllegalArgumentException("x402 challenge protocol or payload is invalid");
        }
        return paymentRequired(required, "x402 challenge");
    }

    private JsonNode paymentRequired(JsonNode value, String label) {
        if (value == null || !value.isObject() || value.path("x402Version").asInt(-1) != 2
                || !value.path("resource").isObject()
                || value.path("resource").path("url").asText("").isBlank()
                || !value.path("accepts").isArray() || value.path("accepts").isEmpty()) {
            throw new IllegalArgumentException(label + " is not an x402 v2 PaymentRequired");
        }
        for (JsonNode accepted : value.path("accepts")) {
            if (!accepted.isObject() || accepted.path("scheme").asText("").isBlank()
                    || accepted.path("network").asText("").isBlank()
                    || accepted.path("amount").asText("").isBlank()
                    || accepted.path("asset").asText("").isBlank()
                    || accepted.path("payTo").asText("").isBlank()
                    || accepted.path("maxTimeoutSeconds").asLong(0) <= 0) {
                throw new IllegalArgumentException(label + " has an invalid x402 v2 payment requirement");
            }
        }
        return value;
    }

    private JsonNode paymentPayload(JsonNode value, String label) {
        if (value == null || !value.isObject() || value.path("x402Version").asInt(-1) != 2
                || !value.path("resource").isObject() || !value.path("accepted").isObject()
                || !value.path("payload").isObject()) {
            throw new IllegalArgumentException(label + " is not an x402 v2 PaymentPayload");
        }
        X402ExactPayload.parse(JSON, value.path("accepted"), value.path("payload"), label);
        return value;
    }

    private JsonNode paymentSettlement(SettledIncomingPayment payment) {
        var receipt = payment.receipt();
        if (!name().equals(receipt.protocol()) || receipt.transactionHash() == null
                || receipt.transactionHash().isBlank() || receipt.payer() == null
                || receipt.payer().isBlank() || receipt.asset() == null
                || receipt.asset().network().isBlank()) {
            throw new IllegalArgumentException("confirmed x402 receipt omitted transaction, payer, or network");
        }
        ObjectNode value = JSON.createObjectNode();
        value.put("success", true);
        value.put("payer", receipt.payer());
        value.put("transaction", receipt.transactionHash());
        value.put("network", receipt.asset().network());
        return value;
    }

    private JsonNode settlement(JsonNode value, String label) {
        if (value == null || !value.isObject() || !value.path("success").asBoolean(false)
                || value.path("payer").asText("").isBlank()
                || value.path("transaction").asText("").isBlank()
                || value.path("network").asText("").isBlank()) {
            throw new IllegalArgumentException(label + " is not a successful x402 settlement");
        }
        return value;
    }

    private ObjectNode paymentResult(JsonNode required) {
        ObjectNode result = JSON.createObjectNode();
        result.putArray("content").addObject()
                .put("type", "text")
                .put("text", compact(required, "x402 MCP challenge"));
        result.set("structuredContent", required);
        result.put("isError", true);
        return result;
    }

    private JsonNode decode(String value, String label) {
        try {
            if (value == null || value.isBlank() || value.indexOf('\r') >= 0 || value.indexOf('\n') >= 0) {
                throw new IllegalArgumentException(label + " is empty or unsafe");
            }
            return JSON.readTree(Base64.getDecoder().decode(value));
        } catch (JsonProcessingException | IllegalArgumentException error) {
            throw new IllegalArgumentException(label + " is invalid", error);
        } catch (java.io.IOException error) {
            throw new IllegalArgumentException(label + " is invalid", error);
        }
    }

    private String encode(JsonNode value, String label) {
        return Base64.getEncoder().encodeToString(jsonBytes(value, label));
    }

    private byte[] jsonBytes(JsonNode value, String label) {
        try {
            return JSON.writeValueAsBytes(value);
        } catch (JsonProcessingException error) {
            throw new IllegalArgumentException(label + " cannot be encoded", error);
        }
    }

    private String compact(JsonNode value, String label) {
        return new String(jsonBytes(value, label), StandardCharsets.UTF_8);
    }

    private void boundedHeader(String value, int limit, String label) {
        if (limit <= 0 || value.length() > (long) Math.ceil(limit * 4.0 / 3.0) + 4
                || value.indexOf('\r') >= 0 || value.indexOf('\n') >= 0) {
            throw new IllegalArgumentException(label + " is invalid or oversized");
        }
    }

    private void boundedJson(JsonNode value, int limit, String label) {
        byte[] encoded = jsonBytes(value, label);
        if (limit <= 0 || encoded.length == 0 || encoded.length > limit) {
            throw new IllegalArgumentException(label + " is empty or oversized");
        }
    }

    private static void requireObject(JsonNode value, String label) {
        if (value == null || !value.isObject()) {
            throw new IllegalArgumentException(label + " must be an object");
        }
    }
}
