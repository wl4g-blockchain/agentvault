package io.agentvault.sdk.model;

import com.fasterxml.jackson.annotation.JsonCreator;
import com.fasterxml.jackson.annotation.JsonInclude;
import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.annotation.JsonValue;
import com.fasterxml.jackson.databind.JsonNode;
import java.util.LinkedHashMap;
import java.util.Map;

public final class Base {
    private Base() {}

    public enum PaymentDirection {
        INBOUND("inbound"), OUTBOUND("outbound");
        private final String wire;
        PaymentDirection(String wire) { this.wire = wire; }
        @JsonValue public String wire() { return wire; }
        @JsonCreator public static PaymentDirection fromWire(String value) {
            for (PaymentDirection candidate : values()) if (candidate.wire.equals(value)) return candidate;
            throw new IllegalArgumentException("invalid payment direction");
        }
    }

    public enum PaymentStatus {
        CREATED("created"), PENDING_APPROVAL("pending_approval"), AUTHORIZED("authorized"),
        SIGNED("signed"), SUBMITTED("submitted"), PENDING_SETTLEMENT("pending_settlement"),
        SETTLED("settled"), FAILED("failed"), EXPIRED("expired"), UNKNOWN("unknown");
        private final String wire;
        PaymentStatus(String wire) { this.wire = wire; }
        @JsonValue public String wire() { return wire; }
        @JsonCreator public static PaymentStatus fromWire(String value) {
            for (PaymentStatus candidate : values()) if (candidate.wire.equals(value)) return candidate;
            throw new IllegalArgumentException("invalid payment status");
        }
    }

    public enum ReservationStatus {
        UNRESERVED("unreserved"), RESERVED("reserved"), CONSUMED("consumed"), RELEASED("released");
        private final String wire;
        ReservationStatus(String wire) { this.wire = wire; }
        @JsonValue public String wire() { return wire; }
        @JsonCreator public static ReservationStatus fromWire(String value) {
            for (ReservationStatus candidate : values()) if (candidate.wire.equals(value)) return candidate;
            throw new IllegalArgumentException("invalid reservation status");
        }
    }

    public record OperationContext(
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("run_id") String runId) {
        public OperationContext {
            required("scenario_id", scenarioId);
            required("run_id", runId);
            if (!runId.matches("[A-Za-z0-9_.:-]{1,128}")) {
                throw new IllegalArgumentException("invalid run_id");
            }
        }
    }

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record AssetIdentity(
            String network,
            String asset,
            @JsonProperty("token_contract") String tokenContract,
            int decimals) {
        public AssetIdentity {
            required("network", network);
            required("asset", asset);
            if (decimals < 0 || decimals > 255) throw new IllegalArgumentException("decimals must be in 0..255");
        }
    }

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record PaymentChallenge(
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("price_id") String priceId,
            String resource,
            String protocol,
            AssetIdentity asset,
            String amount,
            String recipient,
            String nonce,
            @JsonProperty("challenge_hash") String challengeHash,
            @JsonProperty("expires_at") long expiresAt,
            JsonNode payload) {}

    public record HttpCredential(
            @JsonProperty("bound_origin") String boundOrigin,
            @JsonProperty("header_name") String headerName,
            @JsonProperty("header_value") String headerValue) {
        @Override public String toString() {
            return "HttpCredential[boundOrigin=" + boundOrigin + ", headerName=" + headerName + ", value=<redacted>]";
        }
    }

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record PaymentCredential(
            String protocol,
            @JsonProperty("challenge_hash") String challengeHash,
            @JsonProperty("credential_hash") String credentialHash,
            JsonNode payload,
            HttpCredential http) {
        public PaymentCredential {
            required("credential.protocol", protocol);
            required("credential.challenge_hash", challengeHash);
            required("credential.credential_hash", credentialHash);
        }

        @Override public String toString() {
            return "PaymentCredential[protocol=" + protocol + ", credential=<redacted>]";
        }
    }

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record PaymentIntent(
            String id,
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("run_id") String runId,
            @JsonProperty("wallet_id") String walletId,
            @JsonProperty("policy_id") String policyId,
            @JsonProperty("price_id") String priceId,
            PaymentDirection direction,
            String protocol,
            AssetIdentity asset,
            String recipient,
            String amount,
            @JsonProperty("settled_amount") String settledAmount,
            String resource,
            @JsonProperty("challenge_hash") String challengeHash,
            @JsonProperty("request_hash") String requestHash,
            PaymentStatus status,
            @JsonProperty("reservation_status") ReservationStatus reservationStatus,
            @JsonProperty("reserved_amount") String reservedAmount,
            @JsonProperty("tx_hash") String transactionHash,
            @JsonProperty("settlement_ref") String settlementRef,
            @JsonProperty("next_reconcile_at") Long nextReconcileAt,
            @JsonProperty("created_at") long createdAt,
            @JsonProperty("updated_at") long updatedAt,
            @JsonProperty("expires_at") Long expiresAt,
            @JsonProperty("settled_at") Long settledAt) {}

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record PaymentReceipt(
            @JsonProperty("intent_id") String intentId,
            @JsonProperty("scenario_id") String scenarioId,
            PaymentDirection direction,
            String protocol,
            AssetIdentity asset,
            String recipient,
            String payer,
            @JsonProperty("requested_amount") String requestedAmount,
            @JsonProperty("settled_amount") String settledAmount,
            PaymentStatus status,
            @JsonProperty("tx_hash") String transactionHash,
            @JsonProperty("settlement_ref") String settlementRef,
            @JsonProperty("evidence_hash") String evidenceHash,
            @JsonProperty("confirmed_at") long confirmedAt) {}

    public static void required(String name, String value) {
        if (value == null || value.isBlank()) throw new IllegalArgumentException(name + " cannot be blank");
    }

    public static Map<String, Object> contextMap(OperationContext context) {
        var value = new LinkedHashMap<String, Object>();
        value.put("scenario_id", context.scenarioId());
        value.put("run_id", context.runId());
        return value;
    }
}
