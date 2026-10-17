package io.agentvault.sdk.model;

import com.fasterxml.jackson.annotation.JsonCreator;
import com.fasterxml.jackson.annotation.JsonInclude;
import com.fasterxml.jackson.annotation.JsonProperty;
import com.fasterxml.jackson.annotation.JsonValue;
import io.agentvault.sdk.model.Approval.ApprovalRequest;
import io.agentvault.sdk.model.Base.AssetIdentity;
import io.agentvault.sdk.model.Base.PaymentCredential;
import io.agentvault.sdk.model.Base.PaymentIntent;
import java.util.Map;

public final class Payer {
    private Payer() {}

    public enum AuthorizationOutcome {
        AUTHORIZED("authorized"), REQUIRES_APPROVAL("requires_approval"), DENIED("denied");
        private final String wire;
        AuthorizationOutcome(String wire) { this.wire = wire; }
        @JsonValue public String wire() { return wire; }
        @JsonCreator public static AuthorizationOutcome fromWire(String value) {
            for (AuthorizationOutcome candidate : values()) if (candidate.wire.equals(value)) return candidate;
            throw new IllegalArgumentException("invalid authorization outcome");
        }
    }

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record PaymentRequired(
            @JsonProperty("protocol_hint") String protocolHint,
            String origin,
            String resource,
            @JsonProperty("content_type") String contentType,
            Map<String, String> headers,
            String body,
            @JsonProperty("expires_at") String expiresAt) {
        @Override public String toString() {
            return "PaymentRequired[origin=" + origin + ", resource=<redacted>, headers=<redacted>, body=<redacted>]";
        }
    }

    public record AuthorizePaymentRequest(
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("run_id") String runId,
            PaymentRequired challenge) {
        public AuthorizePaymentRequest {
            new Base.OperationContext(scenarioId, runId);
            if (challenge == null) throw new IllegalArgumentException("challenge is required");
            Base.required("challenge.origin", challenge.origin());
            Base.required("challenge.resource", challenge.resource());
            Base.required("challenge.contentType", challenge.contentType());
        }
    }

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record PaymentAuthorizationResult(
            AuthorizationOutcome outcome,
            @JsonProperty("intent_id") String intentId,
            @JsonProperty("approval_id") String approvalId,
            @JsonProperty("reserved_amount") String reservedAmount,
            String reason) {}

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record AuthorizePaymentResponse(
            PaymentAuthorizationResult authorization,
            PaymentIntent intent,
            PaymentCredential credential,
            ApprovalRequest approval) {}

    public record ReportOutboundResponseRequest(
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("run_id") String runId,
            @JsonProperty("payment_response") String paymentResponse) {
        public ReportOutboundResponseRequest {
            new Base.OperationContext(scenarioId, runId);
            if (paymentResponse == null || paymentResponse.length() > 16 * 1024
                    || !paymentResponse.matches("[\\x21-\\x7e]+")) {
                throw new IllegalArgumentException("payment_response must be a bounded official x402 header value");
            }
        }
    }

    public record ReportOutboundResponseResponse(
            PaymentIntent intent,
            boolean accepted,
            boolean replayed) {}

    public record BalanceQuery(String scenarioId, String walletId, AssetIdentity asset) {
        public BalanceQuery {
            Base.required("scenarioId", scenarioId);
            Base.required("walletId", walletId);
            if (asset == null) throw new IllegalArgumentException("asset is required");
        }
    }

    @JsonInclude(JsonInclude.Include.NON_NULL)
    public record PaymentBalance(
            @JsonProperty("scenario_id") String scenarioId,
            @JsonProperty("wallet_id") String walletId,
            AssetIdentity asset,
            @JsonProperty("confirmed_inbound") String confirmedInbound,
            @JsonProperty("confirmed_outbound") String confirmedOutbound,
            @JsonProperty("reserved_outbound") String reservedOutbound,
            String available,
            @JsonProperty("as_of") long asOf) {}

    public record PayerRequestContext(String runId) {
        public PayerRequestContext {
            new Base.OperationContext("validation", runId);
        }

        public Base.OperationContext operationContext(String scenarioId) {
            return new Base.OperationContext(scenarioId, runId);
        }
    }

    public record PayerHttpRequest(
            String method,
            String url,
            String origin,
            Map<String, String> headers,
            byte[] body,
            boolean followRedirects) {
        @Override public String toString() {
            return "PayerHttpRequest[method=" + method + ", url=" + url + ", origin=" + origin
                    + ", headers=<redacted>, body=<redacted>, followRedirects=" + followRedirects + "]";
        }
    }

    public record PayerHttpResponse(int status, Map<String, String> headers, byte[] body) {}

    public record AuthorizedPayment(PaymentIntent intent, PaymentCredential credential) {}
}
