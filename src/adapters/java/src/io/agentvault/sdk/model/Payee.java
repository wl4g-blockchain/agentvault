package io.agentvault.sdk.model;

import com.fasterxml.jackson.annotation.JsonInclude;
import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.databind.JsonNode;
import io.agentvault.sdk.model.Base.AssetIdentity;
import io.agentvault.sdk.model.Base.OperationContext;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Base.PaymentCredential;
import io.agentvault.sdk.model.Base.PaymentIntent;
import io.agentvault.sdk.model.Base.PaymentReceipt;
import java.util.List;

public final class Payee {
    private Payee() {}

    public record ResolveOfferRequest(String scenarioId, String resource, String protocol) {
        public ResolveOfferRequest {
            Base.required("scenarioId", scenarioId);
            Base.required("resource", resource);
            if (protocol != null) Base.required("protocol", protocol);
        }
    }

    public record PaymentOffer(
            String id,
            @JsonProperty("scenario_id") String scenarioId,
            String resource,
            List<String> protocols,
            AssetIdentity asset,
            String amount,
            @JsonProperty("wallet_id") String walletId,
            boolean enabled,
            @JsonProperty("created_at") long createdAt,
            @JsonProperty("updated_at") long updatedAt) {}

    public record PayeeOperation(
            String scenarioId,
            String runId,
            String executionKey,
            String offerId,
            String resource,
            String origin,
            String protocol,
            String requestHash,
            JsonNode credentialPayload) {
        public PayeeOperation {
            for (String value : new String[]{scenarioId, runId, executionKey, offerId, resource,
                    origin, protocol, requestHash}) {
                if (value == null || value.isBlank()) {
                    throw new IllegalArgumentException("payee operation fields cannot be blank");
                }
            }
            if (!executionKey.matches("[A-Za-z0-9_.-]{1,96}")) {
                throw new IllegalArgumentException("invalid execution key");
            }
        }

        public OperationContext challengeContext() {
            return new OperationContext(scenarioId, runId, executionKey + ".challenge");
        }

        public OperationContext settlementContext() {
            return new OperationContext(scenarioId, runId, executionKey + ".settle");
        }
    }

    public record CreateChallengeRequest(
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("run_id") String runId,
            @JsonProperty("idempotency_key") String idempotencyKey,
            @JsonProperty("offer_id") String offerId,
            String resource,
            String origin,
            String protocol,
            @JsonProperty("request_hash") String requestHash) {
        public CreateChallengeRequest(OperationContext context, PayeeOperation operation) {
            this(context.scenarioId(), context.runId(), context.idempotencyKey(), operation.offerId(),
                    operation.resource(), operation.origin(), operation.protocol(), operation.requestHash());
        }
    }

    public record VerifyPaymentRequest(
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("run_id") String runId,
            @JsonProperty("idempotency_key") String idempotencyKey,
            @JsonProperty("challenge_hash") String challengeHash,
            @JsonProperty("request_hash") String requestHash,
            PaymentCredential credential) {
        public VerifyPaymentRequest(OperationContext context, String challengeHash, String requestHash,
                                    PaymentCredential credential) {
            this(context.scenarioId(), context.runId(), context.idempotencyKey(), challengeHash,
                    requestHash, credential);
        }
    }

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record PaymentVerification(
            PaymentIntent intent,
            boolean valid,
            @JsonProperty("verification_ref") String verificationRef) {}

    public record SettlePaymentRequest(
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("run_id") String runId,
            @JsonProperty("idempotency_key") String idempotencyKey,
            @JsonProperty("execution_key") String executionKey,
            @JsonProperty("intent_id") String intentId,
            @JsonProperty("verification_ref") String verificationRef) {
        public SettlePaymentRequest(
                OperationContext context, String executionKey, String intentId, String verificationRef) {
            this(context.scenarioId(), context.runId(), context.idempotencyKey(),
                    executionKey, intentId, verificationRef);
        }
    }

    public record IncomingPayment(
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("run_id") String runId,
            @JsonProperty("offer_id") String offerId,
            String resource,
            @JsonProperty("idempotency_key") String idempotencyKey,
            @JsonProperty("request_hash") String requestHash,
            PaymentCredential credential) {}

    public record SettledIncomingPayment(
            PaymentIntent intent,
            PaymentReceipt receipt,
            @JsonProperty("execution_key") String executionKey) {}

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record HttpProtectionRequest(
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("run_id") String runId,
            @JsonProperty("idempotency_key") String idempotencyKey,
            String resource,
            String origin,
            PaymentCredential credential,
            @JsonProperty("request_hash") String requestHash) {}

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record McpProtectionRequest(
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("run_id") String runId,
            @JsonProperty("idempotency_key") String idempotencyKey,
            String tool,
            @JsonProperty("arguments_hash") String argumentsHash,
            PaymentCredential credential) {}

    public sealed interface ProtectionOutcome permits PaymentRequired, Authorized {}
    public record PaymentRequired(PaymentChallenge challenge) implements ProtectionOutcome {}
    public record Authorized(SettledIncomingPayment payment) implements ProtectionOutcome {}
}
