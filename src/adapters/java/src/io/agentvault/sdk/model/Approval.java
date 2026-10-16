package io.agentvault.sdk.model;

import com.fasterxml.jackson.annotation.JsonCreator;
import com.fasterxml.jackson.annotation.JsonInclude;
import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.annotation.JsonValue;
import io.agentvault.sdk.model.Base.OperationContext;
import java.util.List;

public final class Approval {
    private Approval() {}

    public enum ApprovalStatus {
        PENDING("pending"), APPROVED("approved"), REJECTED("rejected"), EXPIRED("expired"), CONSUMED("consumed");
        private final String wire;
        ApprovalStatus(String wire) { this.wire = wire; }
        @JsonValue public String wire() { return wire; }
        @JsonCreator public static ApprovalStatus fromWire(String value) {
            for (ApprovalStatus candidate : values()) if (candidate.wire.equals(value)) return candidate;
            throw new IllegalArgumentException("invalid approval status");
        }
    }

    public enum ApprovalDecisionKind {
        APPROVE("approve"), REJECT("reject");
        private final String wire;
        ApprovalDecisionKind(String wire) { this.wire = wire; }
        @JsonValue public String wire() { return wire; }
        @JsonCreator public static ApprovalDecisionKind fromWire(String value) {
            for (ApprovalDecisionKind candidate : values()) if (candidate.wire.equals(value)) return candidate;
            throw new IllegalArgumentException("invalid approval decision");
        }
    }

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record ApprovalRequest(
            String id,
            @JsonProperty("intent_id") String intentId,
            @JsonProperty("requested_amount") String requestedAmount,
            @JsonProperty("approved_amount") String approvedAmount,
            @JsonProperty("requested_by") String requestedBy,
            @JsonProperty("approved_by") String approvedBy,
            ApprovalStatus status,
            @JsonProperty("intent_hash") String intentHash,
            @JsonProperty("grant_jti") String grantJti,
            @JsonProperty("grant_claims_hash") String grantClaimsHash,
            @JsonProperty("expires_at") long expiresAt,
            @JsonProperty("created_at") long createdAt,
            @JsonProperty("approved_at") Long approvedAt,
            @JsonProperty("consumed_at") Long consumedAt) {}

    public record ApprovalQuery(@JsonProperty("scenario_id") String scenarioId) {
        public ApprovalQuery { Base.required("scenario_id", scenarioId); }
    }

    public record ApprovalPage(List<ApprovalRequest> items, int total) {}

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record SubmitApprovalDecisionRequest(
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("run_id") String runId,
            @JsonProperty("idempotency_key") String idempotencyKey,
            @JsonProperty("approval_id") String approvalId,
            ApprovalDecisionKind decision,
            @JsonProperty("approved_amount") String approvedAmount,
            String reason) {
        public SubmitApprovalDecisionRequest {
            new OperationContext(scenarioId, runId, idempotencyKey);
            Base.required("approval_id", approvalId);
            if (decision == ApprovalDecisionKind.APPROVE) Base.required("approved_amount", approvedAmount);
            if (decision == ApprovalDecisionKind.REJECT && approvedAmount != null) {
                throw new IllegalArgumentException("approved_amount must be omitted when rejecting");
            }
        }

        public SubmitApprovalDecisionRequest(OperationContext context, String approvalId, ApprovalDecisionKind decision,
                                             String approvedAmount, String reason) {
            this(context.scenarioId(), context.runId(), context.idempotencyKey(), approvalId,
                    decision, approvedAmount, reason);
        }
    }
}
