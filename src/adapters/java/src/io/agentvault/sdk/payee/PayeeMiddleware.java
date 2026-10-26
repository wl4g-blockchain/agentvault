package io.agentvault.sdk.payee;

import com.fasterxml.jackson.databind.JsonNode;

import io.agentvault.sdk.config.Config.PayeeConfig;
import io.agentvault.sdk.model.Base;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Base.PaymentCredential;
import io.agentvault.sdk.model.Base.PaymentReceipt;
import io.agentvault.sdk.model.Payee.Authorized;
import io.agentvault.sdk.model.Payee.CreateChallengeRequest;
import io.agentvault.sdk.model.Payee.PayeeOperation;
import io.agentvault.sdk.model.Payee.PaymentPrice;
import io.agentvault.sdk.model.Payee.PaymentRequired;
import io.agentvault.sdk.model.Payee.PaymentVerification;
import io.agentvault.sdk.model.Payee.ProtectionOutcome;
import io.agentvault.sdk.model.Payee.ResolvePriceRequest;
import io.agentvault.sdk.model.Payee.SettlePaymentRequest;
import io.agentvault.sdk.model.Payee.SettledIncomingPayment;
import io.agentvault.sdk.model.Payee.VerifyPaymentRequest;
import io.agentvault.sdk.protocol.PaymentProtocol;
import io.agentvault.sdk.protocol.PaymentProtocolFactory;
import io.agentvault.sdk.protocol.ProtocolRegistry;
import io.agentvault.sdk.transport.VaultTransport;

/** Transport-neutral inbound challenge, verification, and settlement state machine. */
public final class PayeeMiddleware {
    private static final System.Logger LOG = System.getLogger("io.agentvault.sdk");
    private final VaultTransport transport;
    private final PayeeConfig config;
    private final ProtocolRegistry protocols;

    public PayeeMiddleware(VaultTransport transport, PayeeConfig config) {
        this.transport = java.util.Objects.requireNonNull(transport, "transport");
        this.config = java.util.Objects.requireNonNull(config, "config");
        this.protocols = new PaymentProtocolFactory().create(config.protocols());
    }

    public A2aPayeeMiddleware a2a() {
        return new A2aPayeeMiddleware(this, protocols);
    }

    public PayeeConfig config() {
        return config;
    }

    public PaymentProtocol protocol(String name) {
        return protocols.get(name);
    }

    public java.util.List<String> credentialHeaderNames() {
        return protocols.credentialHeaderNames();
    }

    public java.util.List<String> merchantEvidenceHeaderNames() {
        return protocols.merchantEvidenceHeaderNames();
    }

    public JsonNode presentedCredential(String protocol, java.util.Map<String, String> headers) {
        protocols.get(protocol);
        ProtocolRegistry.McpCredential selected =
                protocols.presentedCredential(headers, config.maxBodyBytes());
        if (selected != null && !selected.protocol().equals(protocol)) {
            throw new IllegalArgumentException(
                    "payment credential protocol does not match the protected route");
        }
        return selected == null ? null : selected.value();
    }

    public JsonNode presentedMcpCredential(String protocol, JsonNode metadata) {
        ProtocolRegistry.McpCredential selected =
                protocols.presentedMcpCredential(metadata, config.maxBodyBytes());
        if (selected != null && !selected.protocol().equals(protocol)) {
            throw new IllegalArgumentException(
                    "MCP payment credential protocol does not match the protected route");
        }
        return selected == null ? null : selected.value();
    }

    public java.util.List<String> mcpRequestMetadataKeys() {
        return protocols.mcpRequestMetadataKeys();
    }

    public java.util.List<String> mcpRootRequestMetadataKeys() {
        return protocols.mcpRootRequestMetadataKeys();
    }

    public java.util.List<String> mcpReceiptMetadataKeys() {
        return protocols.mcpReceiptMetadataKeys();
    }

    public PaymentProtocol.McpPaymentRequired mcpPaymentRequired(
            PaymentChallenge challenge, boolean rejected) {
        PaymentProtocol adapter = protocols.get(challenge.protocol());
        return rejected
                ? adapter.mcpPaymentRejected(challenge, "AgentVault rejected the presented payment")
                : adapter.mcpPaymentRequired(challenge);
    }

    public PaymentProtocol.McpMetadata mcpReceipt(
            PaymentChallenge challenge, SettledIncomingPayment payment) {
        return protocols.get(payment.receipt().protocol()).mcpReceipt(challenge, payment);
    }

    public ProtocolRegistry.McpEvidence mcpMerchantEvidence(JsonNode metadata) {
        return protocols.mcpMerchantEvidence(metadata, config.maxBodyBytes());
    }

    public ProtocolRegistry.McpEvidence merchantEvidence(java.util.Map<String, String> headers) {
        return protocols.merchantEvidence(headers);
    }

    public StandardPayeeMiddleware standard() {
        return new StandardPayeeMiddleware(this);
    }

    public SpringInterceptorPayeeMiddleware spring(
            java.util.List<SpringInterceptorPayeeMiddleware.Route> routes) {
        return new SpringInterceptorPayeeMiddleware(this, routes, config.maxBodyBytes());
    }

    public PaymentChallenge challenge(PayeeOperation operation) {
        PaymentChallenge challenge =
                createChallenge(
                        new CreateChallengeRequest(operation.challengeContext(), operation));
        validateChallengeBinding(operation, challenge);
        return challenge;
    }

    public ProtectionOutcome protect(PayeeOperation operation) {
        event("agentvault.sdk.payee.intercepted", operation.runId(), null);
        PaymentChallenge challenge = challenge(operation);
        event(
                "agentvault.sdk.payee.challenge_created",
                operation.runId(),
                challenge.challengeHash());
        if (operation.credentialPayload() == null) return new PaymentRequired(challenge);
        String challengeHash = challenge.challengeHash();
        Base.required("challenge_hash", challengeHash);
        PaymentCredential credential =
                new PaymentCredential(
                        operation.protocol(),
                        challengeHash,
                        "untrusted",
                        operation.credentialPayload(),
                        null);
        PaymentVerification verification =
                transport.post(
                        "/api/v1/payee/verify",
                        new VerifyPaymentRequest(
                                operation.challengeContext(),
                                challengeHash,
                                operation.requestHash(),
                                credential),
                        PaymentVerification.class);
        if (!verification.valid()) {
            throw new InvalidPaymentException(challenge);
        }
        if (verification.verificationRef() == null || verification.verificationRef().isBlank()) {
            throw new IllegalStateException(
                    "valid AgentVault verification omitted verification_ref");
        }
        validateVerificationBinding(operation, challenge, verification);
        event("agentvault.sdk.payee.verified", operation.runId(), verification.intent().id());
        var settlement = operation.settlementContext();
        SettledIncomingPayment payment =
                transport.post(
                        "/api/v1/payee/settle",
                        new SettlePaymentRequest(
                                settlement,
                                verification.intent().id(),
                                verification.verificationRef()),
                        SettledIncomingPayment.class);
        validateSettlementBinding(operation, verification, payment);
        event("agentvault.sdk.payee.settled", operation.runId(), payment.intent().id());
        return new Authorized(challenge, payment);
    }

    private static void event(String event, String runId, String evidence) {
        LOG.log(
                System.Logger.Level.INFO,
                "{0} run_id={1} evidence={2}",
                event,
                runId,
                evidence == null ? "" : evidence);
    }

    public PaymentPrice resolvePrice(ResolvePriceRequest request) {
        var body = new java.util.LinkedHashMap<String, Object>();
        body.put("scenario_id", request.scenarioId());
        body.put("resource", request.resource());
        if (request.protocol() != null) body.put("protocol", request.protocol());
        return transport.post("/api/v1/prices/resolve", body, PaymentPrice.class);
    }

    public PaymentReceipt getReceipt(String scenarioId, String intentId) {
        Base.required("scenario_id", scenarioId);
        Base.required("intent_id", intentId);
        return transport.get(
                "/api/v1/payments/"
                        + io.agentvault.sdk.payer.PayerMiddleware.escape(intentId)
                        + "/receipt?scenario_id="
                        + io.agentvault.sdk.payer.PayerMiddleware.escape(scenarioId),
                PaymentReceipt.class);
    }

    private PaymentChallenge createChallenge(CreateChallengeRequest request) {
        return transport.post("/api/v1/payee/challenges", request, PaymentChallenge.class);
    }

    private static void validateChallengeBinding(
            PayeeOperation operation, PaymentChallenge challenge) {
        if (!operation.scenarioId().equals(challenge.scenarioId())
                || !operation.priceId().equals(challenge.priceId())
                || !operation.resource().equals(challenge.resource())
                || !operation.protocol().equals(challenge.protocol())
                || challenge.asset() == null
                || challenge.asset().network().isBlank()
                || challenge.asset().asset().isBlank()
                || challenge.amount() == null
                || challenge.amount().isBlank()
                || challenge.recipient() == null
                || challenge.recipient().isBlank()
                || challenge.nonce() == null
                || challenge.nonce().isBlank()
                || challenge.challengeHash() == null
                || challenge.challengeHash().isBlank()) {
            throw new IllegalStateException(
                    "AgentVault challenge is not bound to the protected operation");
        }
    }

    private static void validateVerificationBinding(
            PayeeOperation operation,
            PaymentChallenge challenge,
            PaymentVerification verification) {
        var intent = verification.intent();
        if (intent == null
                || intent.direction() != Base.PaymentDirection.INBOUND
                || !operation.scenarioId().equals(intent.scenarioId())
                || !operation.priceId().equals(intent.priceId())
                || !operation.resource().equals(intent.resource())
                || !operation.protocol().equals(intent.protocol())
                || !challenge.challengeHash().equals(intent.challengeHash())
                || !operation.requestHash().equals(intent.requestHash())
                || !challenge.asset().equals(intent.asset())
                || !challenge.amount().equals(intent.amount())
                || !challenge.recipient().equals(intent.recipient())) {
            throw new IllegalStateException(
                    "AgentVault verification is not bound to the protected operation");
        }
    }

    private static void validateSettlementBinding(
            PayeeOperation operation,
            PaymentVerification verification,
            SettledIncomingPayment payment) {
        var verified = verification.intent();
        if (payment == null
                || payment.intent() == null
                || payment.receipt() == null
                || !verified.id().equals(payment.intent().id())
                || payment.intent().direction() != Base.PaymentDirection.INBOUND
                || !verified.scenarioId().equals(payment.intent().scenarioId())
                || !java.util.Objects.equals(verified.priceId(), payment.intent().priceId())
                || !verified.resource().equals(payment.intent().resource())
                || !verified.protocol().equals(payment.intent().protocol())
                || !java.util.Objects.equals(
                        verified.challengeHash(), payment.intent().challengeHash())
                || !verified.requestHash().equals(payment.intent().requestHash())
                || !verified.asset().equals(payment.intent().asset())
                || !verified.amount().equals(payment.intent().amount())
                || !verified.recipient().equals(payment.intent().recipient())
                || payment.intent().status() != Base.PaymentStatus.SETTLED
                || !payment.intent().id().equals(payment.receipt().intentId())
                || !operation.scenarioId().equals(payment.receipt().scenarioId())
                || payment.receipt().direction() != Base.PaymentDirection.INBOUND
                || !operation.protocol().equals(payment.receipt().protocol())
                || !payment.intent().asset().equals(payment.receipt().asset())
                || !payment.intent().recipient().equals(payment.receipt().recipient())
                || !payment.intent().amount().equals(payment.receipt().requestedAmount())
                || !payment.intent().settledAmount().equals(payment.receipt().settledAmount())
                || payment.receipt().status() != Base.PaymentStatus.SETTLED) {
            throw new IllegalStateException("invalid AgentVault settlement response");
        }
    }

    public static final class InvalidPaymentException extends SecurityException {
        private final PaymentChallenge challenge;

        public InvalidPaymentException(PaymentChallenge challenge) {
            super("AgentVault rejected the presented payment");
            this.challenge = java.util.Objects.requireNonNull(challenge, "challenge");
        }

        public PaymentChallenge challenge() {
            return challenge;
        }
    }
}
