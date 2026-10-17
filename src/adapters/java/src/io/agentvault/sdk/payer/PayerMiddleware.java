package io.agentvault.sdk.payer;

import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.model.Base;
import io.agentvault.sdk.model.Base.OperationContext;
import io.agentvault.sdk.model.Base.PaymentCredential;
import io.agentvault.sdk.model.Base.PaymentIntent;
import io.agentvault.sdk.model.Base.PaymentReceipt;
import io.agentvault.sdk.model.Base.VaultTransport;
import io.agentvault.sdk.model.Payer.AuthorizationOutcome;
import io.agentvault.sdk.model.Payer.AuthorizePaymentRequest;
import io.agentvault.sdk.model.Payer.AuthorizePaymentResponse;
import io.agentvault.sdk.model.Payer.AuthorizedPayment;
import io.agentvault.sdk.model.Payer.BalanceQuery;
import io.agentvault.sdk.model.Payer.PaymentBalance;
import io.agentvault.sdk.model.Payer.PayerHttpRequest;
import io.agentvault.sdk.model.Payer.PayerHttpResponse;
import io.agentvault.sdk.model.Payer.PayerRequestContext;
import io.agentvault.sdk.model.Payer.PaymentRequired;
import io.agentvault.sdk.model.Payer.ReportOutboundResponseRequest;
import io.agentvault.sdk.model.Payer.ReportOutboundResponseResponse;
import java.net.URI;
import java.net.URLEncoder;
import java.nio.ByteBuffer;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.TimeUnit;

/** Transport-neutral payer state machine used by ecosystem HTTP adapters. */
public final class PayerMiddleware {
    private static final Set<String> RESERVED = Set.of(
            "host", "content-length", "authorization", "connection", "transfer-encoding",
            "proxy-authorization", "proxy-authenticate", "upgrade", "te", "trailer",
            "accept", "content-type");

    private final VaultTransport transport;
    private final PayerConfig config;

    public PayerMiddleware(VaultTransport transport, PayerConfig config) {
        this.transport = java.util.Objects.requireNonNull(transport, "transport");
        this.config = java.util.Objects.requireNonNull(config, "config");
    }

    public PayerConfig config() { return config; }

    public StandardPayerMiddleware standard() {
        return new StandardPayerMiddleware(this);
    }

    public AuthorizedPayment authorize(
            PayerHttpRequest request,
            PayerHttpResponse response,
            PayerRequestContext context) {
        if (response.status() != 402) return null;
        if (response.body().length > config.maxChallengeBytes()) {
            throw new IllegalStateException("payment challenge exceeds configured limit");
        }
        URI target = safeUri(request.url());
        if (request.followRedirects() || !origin(target).equalsIgnoreCase(request.origin())) {
            throw new IllegalStateException("paid request must use a canonical origin with redirects disabled");
        }
        var challenge = new PaymentRequired(
                header(response.headers(), "payment-protocol"),
                request.origin(),
                request.url(),
                valueOr(header(response.headers(), "content-type"), "application/json"),
                response.headers(),
                strictUtf8(response.body()),
                header(response.headers(), "payment-expires-at"));
        AuthorizePaymentResponse authorization = authorizeUntilTerminal(new AuthorizePaymentRequest(
                config.scenarioId(), context.runId(), challenge));
        if (authorization.authorization().outcome() == AuthorizationOutcome.DENIED) {
            throw new SecurityException("AgentVault denied the payment: " + authorization.authorization().reason());
        }
        if (authorization.authorization().outcome() != AuthorizationOutcome.AUTHORIZED
                || authorization.credential() == null) {
            throw new IllegalStateException("approval wait returned a non-terminal payment result");
        }
        PaymentCredential credential = authorization.credential();
        PaymentIntent intent = authorization.intent();
        if (intent.id() == null || intent.id().isBlank()
                || !config.scenarioId().equals(intent.scenarioId())
                || !context.runId().equals(intent.runId())
                || intent.walletId() == null || intent.walletId().isBlank()
                || intent.policyId() == null || intent.policyId().isBlank()
                || intent.direction() != Base.PaymentDirection.OUTBOUND
                || (challenge.protocolHint() != null && !challenge.protocolHint().isBlank()
                    && !challenge.protocolHint().equals(intent.protocol()))
                || !credential.protocol().equals(intent.protocol())
                || !request.url().equals(intent.resource())
                || !credential.challengeHash().equals(intent.challengeHash())
                ) {
            throw new IllegalStateException("credential, intent, and resource are not bound to the same payment");
        }
        return new AuthorizedPayment(intent, credential);
    }

    public Map.Entry<String, String> credentialHeader(
            AuthorizedPayment authorized,
            String origin,
            Map<String, String> existing) {
        Base.HttpCredential value = authorized.credential().http();
        if (value == null || !origin.equalsIgnoreCase(value.boundOrigin()) || value.headerName() == null
                || value.headerValue() == null || !value.headerName().matches("[!#$%&'*+\\-.^_`|~A-Za-z0-9]+")
                || value.headerValue().contains("\r") || value.headerValue().contains("\n")
                || RESERVED.contains(value.headerName().toLowerCase())
                || existing.keySet().stream().anyMatch(name -> name.equalsIgnoreCase(value.headerName()))) {
            throw new IllegalStateException("unsafe or missing credential HTTP binding");
        }
        return Map.entry(value.headerName(), value.headerValue());
    }

    public void report(PayerRequestContext context, AuthorizedPayment authorized, PayerHttpResponse response) {
        if (!"x402".equals(authorized.credential().protocol())) return;
        String evidence = header(response.headers(), "payment-response");
        if (evidence != null && !evidence.isBlank()) {
            reportOutboundResponse(
                    new OperationContext(config.scenarioId(), context.runId()),
                    authorized.intent().id(),
                    evidence);
        } else if (response.status() >= 200 && response.status() < 300) {
            throw new IllegalStateException(
                    "successful x402 response omitted PAYMENT-RESPONSE; payment remains reserved");
        }
    }

    private AuthorizePaymentResponse authorizeUntilTerminal(AuthorizePaymentRequest request) {
        long timeoutNanos = config.approvalTimeout().toNanos();
        long started = System.nanoTime();
        String expectedIntent = null;
        for (;;) {
            AuthorizePaymentResponse response = transport.post(
                    "/api/v1/payments/authorize", request, AuthorizePaymentResponse.class);
            if (response.authorization() == null || response.intent() == null) {
                throw new IllegalStateException("invalid AgentVault authorization response");
            }
            AuthorizationOutcome outcome = response.authorization().outcome();
            String intentId = response.authorization().intentId();
            if (outcome == null || intentId == null || intentId.isEmpty() || !intentId.equals(response.intent().id())) {
                throw new IllegalStateException("invalid AgentVault authorization response");
            }
            if (expectedIntent == null) expectedIntent = intentId;
            else if (!expectedIntent.equals(intentId)) {
                throw new IllegalStateException("replayed run changed payment intent");
            }
            if (outcome != AuthorizationOutcome.REQUIRES_APPROVAL) return response;
            long remaining = timeoutNanos - (System.nanoTime() - started);
            if (remaining <= 0) {
                throw new IllegalStateException("payment approval did not complete before the total timeout");
            }
            try {
                TimeUnit.NANOSECONDS.sleep(Math.min(config.approvalPollInterval().toNanos(), remaining));
            } catch (InterruptedException error) {
                Thread.currentThread().interrupt();
                throw new IllegalStateException("payment approval wait interrupted", error);
            }
        }
    }

    public PaymentIntent getPayment(String scenarioId, String intentId) {
        Base.required("scenarioId", scenarioId);
        Base.required("intentId", intentId);
        return transport.get(
                "/api/v1/payments/" + escape(intentId) + "?scenario_id=" + escape(scenarioId), PaymentIntent.class);
    }

    public PaymentReceipt getReceipt(String scenarioId, String intentId) {
        Base.required("scenarioId", scenarioId);
        Base.required("intentId", intentId);
        return transport.get(
                "/api/v1/payments/" + escape(intentId) + "/receipt?scenario_id=" + escape(scenarioId),
                PaymentReceipt.class);
    }

    public PaymentBalance getBalance(BalanceQuery query) {
        String path = "/api/v1/payments/balance?scenario_id=" + escape(query.scenarioId())
                + "&wallet_id=" + escape(query.walletId())
                + "&network=" + escape(query.asset().network())
                + "&asset=" + escape(query.asset().asset())
                + "&decimals=" + query.asset().decimals();
        if (query.asset().tokenContract() != null) {
            path += "&token_contract=" + escape(query.asset().tokenContract());
        }
        return transport.get(path, PaymentBalance.class);
    }

    private ReportOutboundResponseResponse reportOutboundResponse(
            OperationContext context, String intentId, String paymentResponse) {
        Base.required("intent_id", intentId);
        var request = new ReportOutboundResponseRequest(
                context.scenarioId(), context.runId(), paymentResponse);
        return transport.post(
                "/api/v1/payments/" + escape(intentId) + "/outbound-response",
                request,
                ReportOutboundResponseResponse.class);
    }

    static URI safeUri(String value) {
        URI uri = URI.create(value);
        if (!("http".equals(uri.getScheme()) || "https".equals(uri.getScheme()))
                || uri.getHost() == null || uri.getUserInfo() != null) {
            throw new IllegalArgumentException("paid request URL must be absolute HTTP(S) without userinfo");
        }
        return uri;
    }

    static String origin(URI uri) {
        return uri.getScheme() + "://" + uri.getAuthority();
    }

    static String header(Map<String, String> values, String wanted) {
        return values.entrySet().stream()
                .filter(entry -> entry.getKey().equalsIgnoreCase(wanted))
                .map(Map.Entry::getValue)
                .findFirst()
                .orElse(null);
    }

    static Map<String, String> copyHeaders(org.springframework.http.HttpHeaders headers) {
        var result = new LinkedHashMap<String, String>();
        headers.forEach((name, values) -> result.put(name, String.join(", ", values)));
        return result;
    }

    public static String escape(String value) {
        return URLEncoder.encode(value, StandardCharsets.UTF_8).replace("+", "%20");
    }

    private static String valueOr(String value, String fallback) {
        return value == null || value.isBlank() ? fallback : value;
    }

    private static String strictUtf8(byte[] value) {
        try {
            return StandardCharsets.UTF_8.newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(value))
                    .toString();
        } catch (java.nio.charset.CharacterCodingException error) {
            throw new IllegalStateException("payment challenge must be UTF-8", error);
        }
    }
}
