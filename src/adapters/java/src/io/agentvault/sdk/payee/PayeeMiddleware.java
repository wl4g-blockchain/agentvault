package io.agentvault.sdk.payee;

import io.agentvault.sdk.config.Config.PayeeConfig;
import io.agentvault.sdk.model.Base;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Base.PaymentCredential;
import io.agentvault.sdk.model.Base.PaymentReceipt;
import io.agentvault.sdk.model.Base.VaultTransport;
import io.agentvault.sdk.model.Payee.Authorized;
import io.agentvault.sdk.model.Payee.CreateChallengeRequest;
import io.agentvault.sdk.model.Payee.PayeeOperation;
import io.agentvault.sdk.model.Payee.PaymentOffer;
import io.agentvault.sdk.model.Payee.PaymentRequired;
import io.agentvault.sdk.model.Payee.PaymentVerification;
import io.agentvault.sdk.model.Payee.ProtectionOutcome;
import io.agentvault.sdk.model.Payee.ResolveOfferRequest;
import io.agentvault.sdk.model.Payee.SettlePaymentRequest;
import io.agentvault.sdk.model.Payee.SettledIncomingPayment;
import io.agentvault.sdk.model.Payee.VerifyPaymentRequest;

/** Transport-neutral inbound challenge, verification, and settlement state machine. */
public final class PayeeMiddleware {
    private final VaultTransport transport;
    private final PayeeConfig config;

    public PayeeMiddleware(VaultTransport transport, PayeeConfig config) {
        this.transport = java.util.Objects.requireNonNull(transport, "transport");
        this.config = java.util.Objects.requireNonNull(config, "config");
    }

    public PayeeConfig config() { return config; }

    public StandardPayeeMiddleware standard() {
        return new StandardPayeeMiddleware(this);
    }

    public SpringInterceptorPayeeMiddleware spring(java.util.List<SpringInterceptorPayeeMiddleware.Route> routes) {
        return new SpringInterceptorPayeeMiddleware(this, routes, config.maxBodyBytes());
    }

    public PaymentChallenge challenge(PayeeOperation operation) {
        PaymentChallenge challenge = createChallenge(new CreateChallengeRequest(operation.challengeContext(), operation));
        validateChallengeBinding(operation, challenge);
        return challenge;
    }

    public ProtectionOutcome protect(PayeeOperation operation) {
        PaymentChallenge challenge = challenge(operation);
        if (operation.credentialPayload() == null) return new PaymentRequired(challenge);
        String challengeHash = challenge.challengeHash();
        Base.required("challenge_hash", challengeHash);
        PaymentCredential credential = new PaymentCredential(
                operation.protocol(), challengeHash, "untrusted", operation.credentialPayload(), null);
        PaymentVerification verification = transport.post(
                "/api/v1/payee/verify",
                new VerifyPaymentRequest(operation.challengeContext(), challengeHash, operation.requestHash(), credential),
                PaymentVerification.class);
        if (!verification.valid()) {
            throw new SecurityException("AgentVault rejected the presented payment");
        }
        if (verification.verificationRef() == null || verification.verificationRef().isBlank()) {
            throw new IllegalStateException("valid AgentVault verification omitted verification_ref");
        }
        validateVerificationBinding(operation, challenge, verification);
        var settlement = operation.settlementContext();
        SettledIncomingPayment payment = transport.post(
                "/api/v1/payee/settle",
                new SettlePaymentRequest(settlement, verification.intent().id(), verification.verificationRef()),
                SettledIncomingPayment.class);
        validateSettlementBinding(operation, verification, payment);
        return new Authorized(payment);
    }

    public PaymentOffer resolveOffer(ResolveOfferRequest request) {
        var body = new java.util.LinkedHashMap<String, Object>();
        body.put("scenario_id", request.scenarioId());
        body.put("resource", request.resource());
        if (request.protocol() != null) body.put("protocol", request.protocol());
        return transport.post("/api/v1/offers/resolve", body, PaymentOffer.class);
    }

    public PaymentReceipt getReceipt(String scenarioId, String intentId) {
        Base.required("scenario_id", scenarioId);
        Base.required("intent_id", intentId);
        return transport.get(
                "/api/v1/payments/" + io.agentvault.sdk.payer.PayerMiddleware.escape(intentId)
                        + "/receipt?scenario_id=" + io.agentvault.sdk.payer.PayerMiddleware.escape(scenarioId),
                PaymentReceipt.class);
    }

    private PaymentChallenge createChallenge(CreateChallengeRequest request) {
        return transport.post(
                "/api/v1/payee/challenges", request, PaymentChallenge.class);
    }

    private static void validateChallengeBinding(PayeeOperation operation, PaymentChallenge challenge) {
        if (!operation.scenarioId().equals(challenge.scenarioId())
                || !operation.offerId().equals(challenge.offerId())
                || !operation.resource().equals(challenge.resource())
                || !operation.protocol().equals(challenge.protocol())
                || challenge.asset() == null || challenge.asset().network().isBlank()
                || challenge.asset().asset().isBlank() || challenge.amount() == null || challenge.amount().isBlank()
                || challenge.recipient() == null || challenge.recipient().isBlank()
                || challenge.nonce() == null || challenge.nonce().isBlank()
                || challenge.challengeHash() == null || challenge.challengeHash().isBlank()) {
            throw new IllegalStateException("AgentVault challenge is not bound to the protected operation");
        }
    }

    private static void validateVerificationBinding(
            PayeeOperation operation, PaymentChallenge challenge, PaymentVerification verification) {
        var intent = verification.intent();
        if (intent == null || intent.direction() != Base.PaymentDirection.INBOUND
                || !operation.scenarioId().equals(intent.scenarioId())
                || !operation.offerId().equals(intent.offerId())
                || !operation.resource().equals(intent.resource())
                || !operation.protocol().equals(intent.protocol())
                || !challenge.challengeHash().equals(intent.challengeHash())
                || !operation.requestHash().equals(intent.requestHash())
                || !challenge.asset().equals(intent.asset())
                || !challenge.amount().equals(intent.amount())
                || !challenge.recipient().equals(intent.recipient())) {
            throw new IllegalStateException("AgentVault verification is not bound to the protected operation");
        }
    }

    private static void validateSettlementBinding(
            PayeeOperation operation, PaymentVerification verification, SettledIncomingPayment payment) {
        var verified = verification.intent();
        if (payment == null || payment.intent() == null || payment.receipt() == null
                || !verified.id().equals(payment.intent().id())
                || payment.intent().direction() != Base.PaymentDirection.INBOUND
                || !verified.scenarioId().equals(payment.intent().scenarioId())
                || !java.util.Objects.equals(verified.offerId(), payment.intent().offerId())
                || !verified.resource().equals(payment.intent().resource())
                || !verified.protocol().equals(payment.intent().protocol())
                || !java.util.Objects.equals(verified.challengeHash(), payment.intent().challengeHash())
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
}
