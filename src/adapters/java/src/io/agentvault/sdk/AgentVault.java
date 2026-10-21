package io.agentvault.sdk;

import io.agentvault.sdk.config.Config.PayeeConfig;
import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.config.Config.VaultTransportConfig;
import io.agentvault.sdk.model.Approval.ApprovalQuery;
import io.agentvault.sdk.model.Approval.ApprovalPage;
import io.agentvault.sdk.model.Approval.ApprovalRequest;
import io.agentvault.sdk.model.Approval.SubmitApprovalDecisionRequest;
import io.agentvault.sdk.model.Base;
import io.agentvault.sdk.transport.VaultTransport;
import io.agentvault.sdk.payee.PayeeMiddleware;
import io.agentvault.sdk.payer.PayerMiddleware;
import io.agentvault.sdk.transport.VaultTransportFactory;

/** The only SDK facade: construct role middleware from one configured transport. */
public final class AgentVault implements AutoCloseable {
    private final VaultTransport transport;
    private final boolean owned;
    private boolean closed;

    public AgentVault(VaultTransport transport) {
        this(transport, false);
    }

    private AgentVault(VaultTransport transport, boolean owned) {
        this.transport = java.util.Objects.requireNonNull(transport, "transport");
        this.owned = owned;
    }

    public static AgentVault fromConfig(VaultTransportConfig config) {
        VaultTransport transport = new VaultTransportFactory().create(config);
        return new AgentVault(transport, true);
    }

    @Override public synchronized void close() {
        if (closed || !owned) return;
        transport.close();
        closed = true;
    }

    public PayerMiddleware payer(PayerConfig config) {
        return new PayerMiddleware(transport, config);
    }

    public PayeeMiddleware payee(PayeeConfig config) {
        return new PayeeMiddleware(transport, config);
    }

    public PayeeMiddleware payee() {
        return payee(PayeeConfig.defaults());
    }

    public ApprovalPage getPendingApprovals(ApprovalQuery query) {
        return transport.get(
                "/api/v1/approvals?scenario_id=" + PayerMiddleware.escape(query.scenarioId()) + "&status=pending",
                ApprovalPage.class);
    }

    public ApprovalRequest submitApprovalDecision(SubmitApprovalDecisionRequest request) {
        return transport.post(
                "/api/v1/approvals/" + PayerMiddleware.escape(request.approvalId()) + "/decision",
                request,
                ApprovalRequest.class);
    }

    public ApprovalRequest getApprovalStatus(String scenarioId, String approvalId) {
        Base.required("scenario_id", scenarioId);
        Base.required("approval_id", approvalId);
        return transport.get(
                "/api/v1/approvals/" + PayerMiddleware.escape(approvalId)
                        + "?scenario_id=" + PayerMiddleware.escape(scenarioId),
                ApprovalRequest.class);
    }
}
