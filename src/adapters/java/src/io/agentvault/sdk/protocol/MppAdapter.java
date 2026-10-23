package io.agentvault.sdk.protocol;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.stripe.mpp.Challenge;
import com.stripe.mpp.Credential;
import com.stripe.mpp.Json;
import com.stripe.mpp.Receipt;
import com.stripe.mpp.error.VerificationFailedException;
import io.agentvault.sdk.model.Base.HttpCredential;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Base.PaymentCredential;
import io.agentvault.sdk.model.Payee.SettledIncomingPayment;
import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.time.Instant;
import java.util.Base64;
import java.util.List;
import java.util.Map;
import java.util.regex.Matcher;
import java.util.regex.Pattern;

/** MPP Tempo charge wire adapter backed by Stripe's official mpp-java models. */
public final class MppAdapter implements PaymentProtocol {
    public static final String MCP_PAYMENT_REQUIRED = "org.paymentauth/payment-required";
    public static final String MCP_CREDENTIAL = "org.paymentauth/credential";
    public static final String MCP_RECEIPT = "org.paymentauth/receipt";
    public static final int MCP_PAYMENT_REQUIRED_CODE = -32042;
    public static final int MCP_VERIFICATION_FAILED_CODE = -32043;
    private static final ObjectMapper JSON = new ObjectMapper();
    private static final Pattern HEADER_PARAMETER = Pattern.compile(
            "(?i)(?:^|,)\\s*header\\s*=\\s*\"([^\"]*)\"");
    private static final Pattern HEADER_PARAMETER_NAME = Pattern.compile(
            "(?i)(?:^|,)\\s*header\\s*=");

    @Override public String name() { return "mpp"; }

    @Override public java.util.List<String> credentialHeaderNames() {
        return java.util.List.of("payment-authorization", "authorization");
    }

    @Override public java.util.List<String> merchantEvidenceHeaderNames() {
        return java.util.List.of("payment-receipt");
    }

    @Override public HttpCredential credential(PaymentCredential credential) {
        HttpCredential delivery = credential.http();
        String headerName = delivery == null ? "" : delivery.headerName().toLowerCase(java.util.Locale.ROOT);
        if (!name().equals(credential.protocol()) || delivery == null
                || !("authorization".equals(headerName) || "payment-authorization".equals(headerName))) {
            throw new IllegalArgumentException("MPP credential has no allowed Payment authorization delivery");
        }
        Credential parsed = parseCredential(delivery.headerValue(), "MPP credential");
        requireTempoCharge(parsed.challenge().method(), parsed.challenge().intent());
        requireCredentialHeader(delivery.headerValue(), delivery.headerName());
        return delivery;
    }

    @Override public MerchantEvidence merchantEvidence(Map<String, String> headers) {
        String value = header(headers, "payment-receipt");
        if (value == null || value.isBlank()) return null;
        Receipt receipt = parseReceipt(value, "Payment-Receipt");
        requireTempoReceipt(receipt);
        return new MerchantEvidence("payment-receipt", value);
    }

    @Override public JsonNode presentedCredential(Map<String, String> headers, int limit) {
        String dedicated = header(headers, "payment-authorization");
        String authorization = paymentAuthorization(header(headers, "authorization"));
        if (dedicated != null && authorization != null) {
            throw new IllegalArgumentException("multiple MPP payment credentials were supplied");
        }
        String value = dedicated != null ? dedicated : authorization;
        if (value == null) return null;
        boundedHeader(value, limit, "MPP Payment authorization");
        Credential parsed = parseCredential(value, "MPP Payment authorization");
        requireTempoCharge(parsed.challenge().method(), parsed.challenge().intent());
        requireCredentialHeader(value, dedicated != null ? "Payment-Authorization" : "Authorization");
        return JSON.createObjectNode().put("authorization", value);
    }

    @Override public HttpResponse challenge(PaymentChallenge challenge) {
        JsonNode value = challenge.payload() == null ? null : challenge.payload().get("www_authenticate");
        if (!name().equals(challenge.protocol()) || value == null || !value.isTextual()
                || value.textValue().indexOf('\r') >= 0 || value.textValue().indexOf('\n') >= 0) {
            throw new IllegalArgumentException("MPP challenge has no valid WWW-Authenticate value");
        }
        String authenticate = value.textValue();
        supportedChallenge(parseChallenges(authenticate, "MPP challenge"));
        requireSupportedHeaderParameter(authenticate);
        return new HttpResponse(
                Map.of("www-authenticate", authenticate),
                "application/json",
                "{\"error\":{\"code\":\"PAYMENT_REQUIRED\"}}".getBytes(StandardCharsets.UTF_8));
    }

    @Override public HttpResponse receipt(SettledIncomingPayment payment) {
        return new HttpResponse(Map.of(
                "payment-receipt", paymentReceipt(payment).toPaymentReceipt(),
                "cache-control", "private"), null, null);
    }

    @Override public String mcpRequestMetadataKey() { return MCP_CREDENTIAL; }

    @Override public String mcpRootRequestMetadataKey() { return MCP_CREDENTIAL; }

    @Override public String mcpReceiptMetadataKey() { return MCP_RECEIPT; }

    @Override public HttpResponse mcpChallenge(JsonNode message, int limit) {
        boundedJson(message, limit, "MPP MCP message");
        if (message == null || !message.isObject()) return null;
        JsonNode errorData = null;
        JsonNode error = message.get("error");
        if (error != null && error.isObject()
                && (error.path("code").asInt() == MCP_PAYMENT_REQUIRED_CODE
                    || error.path("code").asInt() == MCP_VERIFICATION_FAILED_CODE)) {
            errorData = error.get("data");
            if (errorData == null || !errorData.isObject()) {
                throw new IllegalArgumentException("MPP MCP error omitted challenge data");
            }
        }
        JsonNode resultData = null;
        JsonNode result = message.get("result");
        if (result != null && result.isObject() && result.path("_meta").isObject()) {
            resultData = result.path("_meta").get(MCP_PAYMENT_REQUIRED);
            if (resultData != null && !resultData.isObject()) {
                throw new IllegalArgumentException("MPP MCP result challenge is invalid");
            }
        }
        if (errorData == null && resultData == null) return null;
        if (errorData != null && resultData != null && !errorData.equals(resultData)) {
            throw new IllegalArgumentException("MPP MCP error challenge conflicts with result metadata");
        }
        McpChallenge projection = selectMcpChallenge(errorData != null ? errorData : resultData);
        return new HttpResponse(
                Map.of("www-authenticate", mcpWwwAuthenticate(projection)),
                "application/json",
                "{\"error\":{\"code\":\"PAYMENT_REQUIRED\"}}".getBytes(StandardCharsets.UTF_8));
    }

    @Override public McpMetadata mcpCredential(PaymentCredential credential) {
        HttpCredential delivery = credential(credential);
        return new McpMetadata(MCP_CREDENTIAL, credentialEnvelope(delivery.headerValue()));
    }

    @Override public JsonNode presentedMcpCredential(JsonNode meta, int limit) {
        requireObject(meta, "MCP params._meta");
        JsonNode value = meta.get(MCP_CREDENTIAL);
        if (value == null) return null;
        boundedJson(value, limit, "MPP MCP credential");
        McpCredentialProjection credential = credentialFromMcp(value);
        requireTempoCharge(
                credential.nativeCredential().challenge().method(),
                credential.nativeCredential().challenge().intent());
        return JSON.createObjectNode().put("authorization", credential.authorization());
    }

    @Override public McpPaymentRequired mcpPaymentRequired(PaymentChallenge challenge) {
        McpChallenge stored = storedMcpChallenge(challenge);
        return McpPaymentRequired.error(paymentError(
                MCP_PAYMENT_REQUIRED_CODE, "Payment Required", stored.wire(), null));
    }

    @Override public McpPaymentRequired mcpPaymentRejected(PaymentChallenge challenge, String message) {
        McpChallenge stored = storedMcpChallenge(challenge);
        VerificationFailedException error = new VerificationFailedException(message);
        return McpPaymentRequired.error(paymentError(
                MCP_VERIFICATION_FAILED_CODE,
                error.getMessage(),
                stored.wire(),
                JSON.valueToTree(error.toProblemDetails(stored.nativeChallenge().id()))));
    }

    @Override public McpMetadata mcpReceipt(
            PaymentChallenge challenge, SettledIncomingPayment payment) {
        if (!name().equals(challenge.protocol())
                || !challenge.challengeHash().equals(payment.intent().challengeHash())) {
            throw new IllegalArgumentException("MPP MCP receipt challenge binding is invalid");
        }
        McpChallenge stored = storedMcpChallenge(challenge);
        Receipt nativeReceipt = paymentReceipt(payment);
        ObjectNode value = receiptNode(nativeReceipt);
        value.put("challengeId", stored.nativeChallenge().id());
        return new McpMetadata(MCP_RECEIPT, value);
    }

    @Override public MerchantEvidence mcpMerchantEvidence(JsonNode meta, int limit) {
        requireObject(meta, "MCP result._meta");
        JsonNode value = meta.get(MCP_RECEIPT);
        if (value == null) return null;
        boundedJson(value, limit, "MPP MCP receipt metadata");
        Receipt receipt = receiptFromMcp(value);
        requireTempoReceipt(receipt);
        if (value.path("challengeId").asText("").isBlank()) {
            throw new IllegalArgumentException("MPP MCP receipt metadata is invalid");
        }
        return new MerchantEvidence("payment-receipt", receipt.toPaymentReceipt());
    }

    private McpChallenge storedMcpChallenge(PaymentChallenge challenge) {
        JsonNode value = challenge.payload() == null ? null : challenge.payload().get("mcp_challenge");
        if (!name().equals(challenge.protocol()) || value == null) {
            throw new IllegalArgumentException("MPP challenge protocol or payload is invalid");
        }
        McpChallenge parsed = mcpChallenge(value);
        requireTempoCharge(parsed.nativeChallenge().method(), parsed.nativeChallenge().intent());
        return parsed;
    }

    private McpChallenge selectMcpChallenge(JsonNode data) {
        if (data == null || !data.isObject() || data.path("httpStatus").asInt(-1) != 402
                || !data.path("challenges").isArray() || data.path("challenges").isEmpty()) {
            throw new IllegalArgumentException(
                    "MPP MCP challenge must carry HTTP 402 and at least one challenge");
        }
        for (JsonNode value : data.path("challenges")) {
            try {
                McpChallenge parsed = mcpChallenge(value);
                if ("tempo".equals(parsed.nativeChallenge().method())
                        && "charge".equals(parsed.nativeChallenge().intent())) return parsed;
            } catch (IllegalArgumentException ignored) {
                // Unsupported challenges remain available to another configured implementation.
            }
        }
        throw new IllegalArgumentException("MPP MCP challenge has no supported Tempo charge");
    }

    @SuppressWarnings("unchecked")
    private McpChallenge mcpChallenge(JsonNode value) {
        requireObject(value, "MPP MCP challenge");
        JsonNode request = value.get("request");
        if (request == null || !request.isObject()) {
            throw new IllegalArgumentException("MPP MCP challenge request must be an object");
        }
        String header = value.has("header") ? text(value, "header") : null;
        if (header != null && !"payment-authorization".equalsIgnoreCase(header)) {
            throw new IllegalArgumentException("MPP challenge selected an unsupported credential header");
        }
        Map<String, Object> requestMap = JSON.convertValue(request, Map.class);
        Map<String, Object> opaque = null;
        if (value.has("opaque") && !value.get("opaque").isNull()) {
            if (!value.get("opaque").isObject()) {
                throw new IllegalArgumentException("MPP MCP challenge opaque value must be an object");
            }
            opaque = JSON.convertValue(value.get("opaque"), Map.class);
        }
        Challenge challenge = new Challenge(
                text(value, "id"),
                text(value, "method"),
                text(value, "intent"),
                requestMap,
                text(value, "realm"),
                Base64.getUrlEncoder().withoutPadding().encodeToString(
                        Json.compact(requestMap).getBytes(StandardCharsets.UTF_8)),
                optionalText(value, "digest"),
                optionalText(value, "expires"),
                optionalText(value, "description"),
                opaque);
        requireTempoCharge(challenge.method(), challenge.intent());
        ObjectNode wire = value.deepCopy();
        if (header != null) wire.put("header", "Payment-Authorization");
        return new McpChallenge(challenge, wire);
    }

    private McpCredentialProjection credentialFromMcp(JsonNode value) {
        requireObject(value, "MPP MCP credential");
        JsonNode challengeValue = value.get("challenge");
        JsonNode payload = value.get("payload");
        if (challengeValue == null || payload == null) {
            throw new IllegalArgumentException("MPP MCP credential is invalid");
        }
        McpChallenge challenge = mcpChallenge(challengeValue);
        ObjectNode envelope = value.deepCopy();
        ((ObjectNode) envelope.get("challenge"))
                .put("request", challenge.nativeChallenge().requestB64());
        String authorization = authorizationFromEnvelope(envelope);
        Credential credential = parseCredential(authorization, "MPP MCP credential");
        return new McpCredentialProjection(credential, authorization);
    }

    private ObjectNode credentialEnvelope(String authorization) {
        try {
            ObjectNode envelope = authorizationEnvelope(authorization, "MPP credential");
            ObjectNode challenge = (ObjectNode) envelope.get("challenge");
            byte[] requestJson = Base64.getUrlDecoder().decode(padBase64(text(challenge, "request")));
            challenge.set("request", JSON.valueToTree(
                    Json.parseMap(new String(requestJson, StandardCharsets.UTF_8))));
            return envelope;
        } catch (IllegalArgumentException error) {
            throw new IllegalArgumentException("MPP credential cannot be projected to MCP metadata", error);
        }
    }

    private ObjectNode authorizationEnvelope(String value, String label) {
        parseCredential(value, label);
        String payment = paymentAuthorization(value);
        if (payment == null) throw new IllegalArgumentException(label + " is not a Payment authorization");
        try {
            byte[] decoded = Base64.getUrlDecoder().decode(padBase64(payment.substring(8).strip()));
            JsonNode envelope = JSON.readTree(decoded);
            requireObject(envelope, label);
            requireObject(envelope.get("challenge"), label + " challenge");
            challengedHeader(envelope.get("challenge"));
            return (ObjectNode) envelope;
        } catch (IOException | IllegalArgumentException error) {
            throw new IllegalArgumentException(label + " has an invalid wire envelope", error);
        }
    }

    private String authorizationFromEnvelope(ObjectNode envelope) {
        try {
            return "Payment " + Base64.getUrlEncoder().withoutPadding()
                    .encodeToString(JSON.writeValueAsBytes(envelope));
        } catch (JsonProcessingException error) {
            throw new IllegalArgumentException("MPP MCP credential cannot be encoded", error);
        }
    }

    private void requireCredentialHeader(String authorization, String actual) {
        String expected = challengedHeader(
                authorizationEnvelope(authorization, "MPP credential").get("challenge"));
        if (!expected.equalsIgnoreCase(actual)) {
            throw new IllegalArgumentException(
                    "MPP credential delivery does not match its challenged header");
        }
    }

    private String challengedHeader(JsonNode challenge) {
        JsonNode header = challenge.get("header");
        if (header == null || header.isNull()) return "Authorization";
        if (!header.isTextual() || !"payment-authorization".equalsIgnoreCase(header.textValue())) {
            throw new IllegalArgumentException("MPP credential selected an unsupported credential header");
        }
        return "Payment-Authorization";
    }

    private void requireSupportedHeaderParameter(String authenticate) {
        Matcher name = HEADER_PARAMETER_NAME.matcher(authenticate);
        Matcher value = HEADER_PARAMETER.matcher(authenticate);
        int names = 0;
        while (name.find()) names++;
        int values = 0;
        while (value.find()) {
            values++;
            if (!"payment-authorization".equalsIgnoreCase(value.group(1))) {
                throw new IllegalArgumentException(
                        "MPP challenge selected an unsupported credential header");
            }
        }
        if (names != values) {
            throw new IllegalArgumentException("MPP challenge selected an invalid credential header");
        }
    }

    private String mcpWwwAuthenticate(McpChallenge challenge) {
        String value = challenge.nativeChallenge().toWwwAuthenticate();
        if (challenge.wire().has("header")) value += ", header=\"Payment-Authorization\"";
        return value;
    }

    private ObjectNode paymentError(
            int code, String message, ObjectNode challenge, JsonNode problem) {
        ObjectNode data = JSON.createObjectNode();
        data.put("httpStatus", 402);
        data.putArray("challenges").add(challenge);
        if (problem != null) data.set("problem", problem);
        ObjectNode error = JSON.createObjectNode();
        error.put("code", code);
        error.put("message", message);
        error.set("data", data);
        return error;
    }

    private Receipt paymentReceipt(SettledIncomingPayment payment) {
        var receipt = payment.receipt();
        if (!name().equals(receipt.protocol()) || receipt.transactionHash() == null
                || receipt.transactionHash().isBlank() || receipt.confirmedAt() <= 0) {
            throw new IllegalArgumentException(
                    "confirmed MPP receipt omitted transaction or confirmation time");
        }
        return new Receipt("success", Instant.ofEpochSecond(receipt.confirmedAt()),
                receipt.transactionHash(), "tempo", payment.intent().id(), null);
    }

    private Receipt receiptFromMcp(JsonNode value) {
        requireObject(value, "MPP MCP receipt metadata");
        try {
            return new Receipt(
                    text(value, "status"),
                    Instant.parse(text(value, "timestamp")),
                    text(value, "reference"),
                    text(value, "method"),
                    optionalText(value, "externalId"),
                    value.has("extra") ? JSON.convertValue(value.get("extra"), Object.class) : null);
        } catch (RuntimeException error) {
            throw new IllegalArgumentException("MPP MCP receipt metadata is invalid", error);
        }
    }

    private ObjectNode receiptNode(Receipt receipt) {
        ObjectNode value = JSON.createObjectNode();
        value.put("status", receipt.status());
        value.put("method", receipt.method());
        value.put("timestamp", receipt.timestamp().toString());
        value.put("reference", receipt.reference());
        if (receipt.externalId() != null) value.put("externalId", receipt.externalId());
        if (receipt.extra() != null) value.set("extra", JSON.valueToTree(receipt.extra()));
        return value;
    }

    private List<Challenge> parseChallenges(String value, String label) {
        try {
            return Challenge.fromWwwAuthenticate(value);
        } catch (RuntimeException error) {
            throw new IllegalArgumentException(label + " has no valid WWW-Authenticate value", error);
        }
    }

    private Challenge supportedChallenge(List<Challenge> challenges) {
        return challenges.stream()
                .filter(value -> "tempo".equals(value.method()) && "charge".equals(value.intent()))
                .findFirst()
                .orElseThrow(() -> new IllegalArgumentException(
                        "MPP supports only the implemented Tempo charge flow"));
    }

    private Credential parseCredential(String value, String label) {
        try {
            if (value == null || !value.regionMatches(true, 0, "Payment ", 0, 8)) {
                throw new IllegalArgumentException("Payment scheme is missing");
            }
            return Credential.fromAuthorization(value);
        } catch (RuntimeException error) {
            throw new IllegalArgumentException(label + " is not a valid Payment authorization", error);
        }
    }

    private Receipt parseReceipt(String value, String label) {
        try {
            return Receipt.fromPaymentReceipt(value);
        } catch (RuntimeException error) {
            throw new IllegalArgumentException(label + " is invalid", error);
        }
    }

    private static void requireTempoCharge(String method, String intent) {
        if (!"tempo".equals(method) || !"charge".equals(intent)) {
            throw new IllegalArgumentException("MPP supports only the implemented Tempo charge flow");
        }
    }

    private static void requireTempoReceipt(Receipt receipt) {
        if (!"success".equals(receipt.status()) || !"tempo".equals(receipt.method())
                || receipt.reference() == null || receipt.reference().isBlank()) {
            throw new IllegalArgumentException(
                    "MPP Payment-Receipt is not a successful Tempo receipt");
        }
    }

    private static String paymentAuthorization(String value) {
        if (value == null || !value.regionMatches(true, 0, "Payment ", 0, 8)) return null;
        return "Payment " + value.substring(8).stripLeading();
    }

    private static String padBase64(String value) {
        return value + "=".repeat((4 - value.length() % 4) % 4);
    }

    private static String text(JsonNode value, String name) {
        String result = value.path(name).asText("");
        if (result.isBlank()) throw new IllegalArgumentException("MPP field is required: " + name);
        return result;
    }

    private static String optionalText(JsonNode value, String name) {
        JsonNode field = value.get(name);
        if (field == null || field.isNull()) return null;
        if (!field.isTextual()) throw new IllegalArgumentException("MPP field must be text: " + name);
        return field.textValue();
    }

    private static void requireObject(JsonNode value, String label) {
        if (value == null || !value.isObject()) {
            throw new IllegalArgumentException(label + " must be an object");
        }
    }

    private void boundedHeader(String value, int limit, String label) {
        if (limit <= 0 || value.length() > limit || value.indexOf('\r') >= 0 || value.indexOf('\n') >= 0) {
            throw new IllegalArgumentException(label + " is invalid or oversized");
        }
    }

    private void boundedJson(JsonNode value, int limit, String label) {
        try {
            byte[] encoded = JSON.writeValueAsBytes(value);
            if (limit <= 0 || encoded.length == 0 || encoded.length > limit) {
                throw new IllegalArgumentException(label + " is empty or oversized");
            }
        } catch (JsonProcessingException error) {
            throw new IllegalArgumentException(label + " is invalid", error);
        }
    }

    private record McpChallenge(Challenge nativeChallenge, ObjectNode wire) {}
    private record McpCredentialProjection(Credential nativeCredential, String authorization) {}
}
