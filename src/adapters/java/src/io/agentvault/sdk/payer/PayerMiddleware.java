package io.agentvault.sdk.payer;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;

import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.model.Base;
import io.agentvault.sdk.model.Base.OperationContext;
import io.agentvault.sdk.model.Base.PaymentCredential;
import io.agentvault.sdk.model.Base.PaymentIntent;
import io.agentvault.sdk.model.Base.PaymentReceipt;
import io.agentvault.sdk.model.Payer.AuthorizationOutcome;
import io.agentvault.sdk.model.Payer.AuthorizePaymentRequest;
import io.agentvault.sdk.model.Payer.AuthorizePaymentResponse;
import io.agentvault.sdk.model.Payer.AuthorizedPayment;
import io.agentvault.sdk.model.Payer.BalanceQuery;
import io.agentvault.sdk.model.Payer.PayerHttpRequest;
import io.agentvault.sdk.model.Payer.PayerHttpResponse;
import io.agentvault.sdk.model.Payer.PayerRequestContext;
import io.agentvault.sdk.model.Payer.PaymentBalance;
import io.agentvault.sdk.model.Payer.PaymentRequired;
import io.agentvault.sdk.model.Payer.ProtocolEvidence;
import io.agentvault.sdk.model.Payer.ReportOutboundResponseRequest;
import io.agentvault.sdk.model.Payer.ReportOutboundResponseResponse;
import io.agentvault.sdk.protocol.PaymentProtocolFactory;
import io.agentvault.sdk.protocol.ProtocolRegistry;
import io.agentvault.sdk.transport.VaultTransport;

import java.net.URI;
import java.net.URLEncoder;
import java.nio.ByteBuffer;
import java.nio.charset.CodingErrorAction;
import java.nio.charset.StandardCharsets;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.Set;
import java.util.concurrent.CompletableFuture;
import java.util.concurrent.CompletionStage;
import java.util.concurrent.TimeUnit;

/** Transport-neutral payer state machine used by ecosystem HTTP adapters. */
public final class PayerMiddleware {
    private static final System.Logger LOG = System.getLogger("io.agentvault.sdk");
    private static final ObjectMapper JSON = new ObjectMapper();
    private static final Set<String> RESERVED =
            Set.of(
                    "host",
                    "content-length",
                    "connection",
                    "transfer-encoding",
                    "proxy-authorization",
                    "proxy-authenticate",
                    "upgrade",
                    "te",
                    "trailer",
                    "accept",
                    "content-type");

    private final VaultTransport transport;
    private final PayerConfig config;
    private final ProtocolRegistry protocols;

    public PayerMiddleware(VaultTransport transport, PayerConfig config) {
        this.transport = java.util.Objects.requireNonNull(transport, "transport");
        this.config = java.util.Objects.requireNonNull(config, "config");
        this.protocols = new PaymentProtocolFactory().create(config.protocols());
    }

    public A2aPayerMiddleware a2a() {
        return new A2aPayerMiddleware(this, protocols);
    }

    public PayerConfig config() {
        return config;
    }

    public SpringRestPayerMiddleware spring() {
        return new SpringRestPayerMiddleware(this);
    }

    public JdkHttpPayerMiddleware jdkHttp() {
        return new JdkHttpPayerMiddleware(this);
    }

    public AuthorizedPayment authorize(
            PayerHttpRequest request, PayerHttpResponse response, PayerRequestContext context) {
        if (response.status() != 402) return null;
        if (response.body().length > config.maxChallengeBytes()) {
            throw new IllegalStateException("payment challenge exceeds configured limit");
        }
        URI target = safeUri(request.url());
        if (request.followRedirects() || !origin(target).equalsIgnoreCase(request.origin())) {
            throw new IllegalStateException(
                    "paid request must use a canonical origin with redirects disabled");
        }
        var challenge =
                new PaymentRequired(
                        header(response.headers(), "payment-protocol"),
                        request.origin(),
                        request.url(),
                        valueOr(header(response.headers(), "content-type"), "application/json"),
                        response.headers(),
                        strictUtf8(response.body()),
                        header(response.headers(), "payment-expires-at"));
        if (challenge.protocolHint() != null && !challenge.protocolHint().isBlank()) {
            protocols.get(challenge.protocolHint());
        }
        AuthorizePaymentResponse authorization =
                authorizeUntilTerminal(
                        new AuthorizePaymentRequest(
                                config.scenarioId(), context.runId(), challenge));
        if (authorization.authorization().outcome() == AuthorizationOutcome.DENIED) {
            throw new SecurityException(
                    "AgentVault denied the payment: " + authorization.authorization().reason());
        }
        if (authorization.authorization().outcome() != AuthorizationOutcome.AUTHORIZED
                || authorization.credential() == null) {
            throw new IllegalStateException("approval wait returned a non-terminal payment result");
        }
        PaymentCredential credential = authorization.credential();
        PaymentIntent intent = authorization.intent();
        protocols.get(intent.protocol());
        if (intent.id() == null
                || intent.id().isBlank()
                || !config.scenarioId().equals(intent.scenarioId())
                || !context.runId().equals(intent.runId())
                || intent.walletId() == null
                || intent.walletId().isBlank()
                || intent.policyId() == null
                || intent.policyId().isBlank()
                || intent.direction() != Base.PaymentDirection.OUTBOUND
                || (challenge.protocolHint() != null
                        && !challenge.protocolHint().isBlank()
                        && !challenge.protocolHint().equals(intent.protocol()))
                || !credential.protocol().equals(intent.protocol())
                || !request.url().equals(intent.resource())
                || !credential.challengeHash().equals(intent.challengeHash())) {
            throw new IllegalStateException(
                    "credential, intent, and resource are not bound to the same payment");
        }
        return new AuthorizedPayment(intent, credential);
    }

    public AuthorizedPayment authorizeMessage(
            PayerHttpRequest request,
            JsonNode message,
            PayerRequestContext context,
            Map<String, String> headers) {
        return io.agentvault.sdk.model.A2a.parse(request.body(), config.maxChallengeBytes()) != null
                ? a2a().authorize(request, message, context, headers)
                : authorizeMcp(request, message, context, headers);
    }

    public AuthorizedPayment authorizeMcp(
            PayerHttpRequest request, JsonNode message, PayerRequestContext context) {
        return authorizeMcp(request, message, context, Map.of());
    }

    public AuthorizedPayment authorizeMcp(
            PayerHttpRequest request,
            JsonNode message,
            PayerRequestContext context,
            Map<String, String> responseHeaders) {
        JsonNode requestId = mcpCallId(request.body());
        if (requestId == null || !boundMcpResponse(message, requestId, false)) return null;
        ProtocolRegistry.McpChallenge selected =
                protocols.mcpChallenge(message, config.maxChallengeBytes());
        if (selected == null) return null;
        if (header(responseHeaders, "payment-required") != null) {
            throw new IllegalArgumentException(
                    "MCP payment challenge must not also declare PAYMENT-REQUIRED");
        }
        String advertised = header(responseHeaders, "payment-protocol");
        if (advertised != null && !advertised.equalsIgnoreCase(selected.adapter().name())) {
            throw new IllegalArgumentException(
                    "MCP payment challenge protocol metadata conflicts with its JSON-RPC payload");
        }
        var headers = new LinkedHashMap<>(selected.challenge().headers());
        headers.put("payment-protocol", selected.adapter().name());
        String expiresAt = header(responseHeaders, "payment-expires-at");
        if (expiresAt != null && !expiresAt.isBlank()) {
            headers.put("payment-expires-at", expiresAt);
        }
        return authorize(
                request, new PayerHttpResponse(402, headers, selected.challenge().body()), context);
    }

    private static JsonNode mcpCallId(byte[] body) {
        try {
            JsonNode message = JSON.readTree(body);
            if (message == null
                    || !message.isObject()
                    || !"2.0".equals(message.path("jsonrpc").asText())
                    || !"tools/call".equals(message.path("method").asText())
                    || !message.path("params").isObject()
                    || !(message.path("id").isTextual() || message.path("id").isNumber()))
                return null;
            return message.get("id");
        } catch (java.io.IOException error) {
            return null;
        }
    }

    static boolean boundMcpResponse(JsonNode message, JsonNode expectedId, boolean requireResult) {
        return message != null
                && message.isObject()
                && "2.0".equals(message.path("jsonrpc").asText())
                && expectedId.equals(message.get("id"))
                && (!requireResult || message.path("result").isObject());
    }

    public io.agentvault.sdk.protocol.PaymentProtocol.McpMetadata mcpCredential(
            AuthorizedPayment authorized, String origin) {
        if (!authorized.intent().protocol().equals(authorized.credential().protocol())) {
            throw new IllegalStateException("outbound payment protocol binding is invalid");
        }
        var adapter = protocols.get(authorized.credential().protocol());
        Base.HttpCredential delivery = adapter.credential(authorized.credential());
        if (!origin.equalsIgnoreCase(delivery.boundOrigin())) {
            throw new IllegalStateException("unsafe or missing MCP credential origin binding");
        }
        return adapter.mcpCredential(authorized.credential());
    }

    public List<String> mcpRequestMetadataKeys() {
        return protocols.mcpRequestMetadataKeys();
    }

    public List<String> mcpRootRequestMetadataKeys() {
        return protocols.mcpRootRequestMetadataKeys();
    }

    public Map.Entry<String, String> credentialHeader(
            AuthorizedPayment authorized, String origin, Map<String, String> existing) {
        Base.HttpCredential value =
                protocols
                        .get(authorized.credential().protocol())
                        .credential(authorized.credential());
        if (!origin.equalsIgnoreCase(value.boundOrigin())
                || value.headerName() == null
                || value.headerValue() == null
                || !value.headerName().matches("[!#$%&'*+\\-.^_`|~A-Za-z0-9]+")
                || value.headerValue().contains("\r")
                || value.headerValue().contains("\n")
                || RESERVED.contains(value.headerName().toLowerCase())
                || existing.keySet().stream()
                        .anyMatch(name -> name.equalsIgnoreCase(value.headerName()))) {
            throw new IllegalStateException("unsafe or missing credential HTTP binding");
        }
        return Map.entry(value.headerName(), value.headerValue());
    }

    public void report(
            PayerRequestContext context, AuthorizedPayment authorized, PayerHttpResponse response) {
        if (!authorized.intent().protocol().equals(authorized.credential().protocol())) {
            throw new IllegalStateException("outbound payment protocol binding is invalid");
        }
        var adapter = protocols.get(authorized.credential().protocol());
        ProtocolRegistry.McpEvidence selected = protocols.merchantEvidence(response.headers());
        if (selected != null && !selected.protocol().equals(adapter.name())) {
            throw new IllegalStateException(
                    "HTTP settlement receipt protocol does not match the paid credential");
        }
        var evidence = selected == null ? null : selected.evidence();
        if (evidence != null) {
            reportOutboundResponse(
                    new OperationContext(config.scenarioId(), context.runId()),
                    authorized.intent().id(),
                    adapter.name(),
                    evidence);
        } else if (response.status() >= 200 && response.status() < 300) {
            throw new IllegalStateException(
                    "successful "
                            + adapter.name()
                            + " response omitted settlement evidence; payment remains reserved");
        }
    }

    public void reportMcp(
            PayerRequestContext context, AuthorizedPayment authorized, JsonNode metadata) {
        if (!authorized.intent().protocol().equals(authorized.credential().protocol())) {
            throw new IllegalStateException("outbound payment protocol binding is invalid");
        }
        ProtocolRegistry.McpEvidence selected =
                protocols.mcpMerchantEvidence(metadata, config.maxChallengeBytes());
        if (selected == null || !authorized.intent().protocol().equals(selected.protocol())) {
            throw new IllegalStateException(
                    "successful "
                            + authorized.intent().protocol()
                            + " MCP response omitted matching settlement evidence; payment remains"
                            + " reserved");
        }
        reportOutboundResponse(
                new OperationContext(config.scenarioId(), context.runId()),
                authorized.intent().id(),
                selected.protocol(),
                selected.evidence());
    }

    @FunctionalInterface
    public interface Delivery<T, E extends Exception> {
        T send(int attemptNo) throws E;
    }

    public interface DeliveryResponse<T, E extends Exception> {
        PayerHttpResponse view(T response) throws E;

        void discard(T response) throws E;
    }

    @FunctionalInterface
    public interface DeliveryReporter<T, E extends Exception> {
        void report(T response) throws E;
    }

    /** Owns the bounded paid-request retry state machine for synchronous adapters. */
    public <T, E extends Exception> T deliver(
            PayerRequestContext context,
            AuthorizedPayment authorized,
            Delivery<T, E> delivery,
            DeliveryResponse<T, E> responseAdapter)
            throws E {
        return deliver(
                context,
                authorized,
                delivery,
                responseAdapter,
                response -> report(context, authorized, responseAdapter.view(response)));
    }

    /** Same bounded state machine with protocol-specific response evidence extraction. */
    public <T, E extends Exception> T deliver(
            PayerRequestContext context,
            AuthorizedPayment authorized,
            Delivery<T, E> delivery,
            DeliveryResponse<T, E> responseAdapter,
            DeliveryReporter<T, E> reporter)
            throws E {
        for (int attempt = 0; attempt <= config.deliveryRetries(); attempt++) {
            deliveryEvent(context, authorized, attempt + 1);
            T response = delivery.send(attempt + 1);
            PayerHttpResponse view = responseAdapter.view(response);
            try {
                reporter.report(response);
            } catch (Exception error) {
                responseAdapter.discard(response);
                @SuppressWarnings("unchecked")
                E typed = (E) error;
                throw typed;
            }
            if (view.status() != 503 || attempt == config.deliveryRetries()) return response;
            responseAdapter.discard(response);
        }
        throw new IllegalStateException("payer retry loop ended unexpectedly");
    }

    public interface AsyncDelivery<T> {
        CompletionStage<T> send(int attemptNo);

        PayerHttpResponse view(T response);

        CompletionStage<Void> discard(T response);
    }

    @FunctionalInterface
    public interface AsyncDeliveryReporter<T> {
        CompletionStage<Void> report(T response);
    }

    /** Owns the same state machine for WebFlux and other asynchronous adapters. */
    public <T> CompletionStage<T> deliverAsync(
            PayerRequestContext context, AuthorizedPayment authorized, AsyncDelivery<T> delivery) {
        return deliverAsync(
                context,
                authorized,
                delivery,
                response -> {
                    report(context, authorized, delivery.view(response));
                    return CompletableFuture.completedFuture(null);
                });
    }

    public <T> CompletionStage<T> deliverAsync(
            PayerRequestContext context,
            AuthorizedPayment authorized,
            AsyncDelivery<T> delivery,
            AsyncDeliveryReporter<T> reporter) {
        return deliverAsync(context, authorized, delivery, reporter, 0);
    }

    private <T> CompletionStage<T> deliverAsync(
            PayerRequestContext context,
            AuthorizedPayment authorized,
            AsyncDelivery<T> delivery,
            AsyncDeliveryReporter<T> reporter,
            int attempt) {
        if (attempt > config.deliveryRetries()) {
            return CompletableFuture.failedFuture(
                    new IllegalStateException("payer retry loop ended unexpectedly"));
        }
        deliveryEvent(context, authorized, attempt + 1);
        final CompletionStage<T> sent;
        try {
            sent = delivery.send(attempt + 1);
        } catch (RuntimeException error) {
            return CompletableFuture.failedFuture(error);
        }
        return sent.thenCompose(
                response -> {
                    PayerHttpResponse view = delivery.view(response);
                    CompletionStage<Void> reported;
                    try {
                        reported = reporter.report(response);
                    } catch (RuntimeException error) {
                        reported = CompletableFuture.failedFuture(error);
                    }
                    return reported.handle(
                                    (ignored, error) -> {
                                        if (error == null)
                                            return CompletableFuture.completedFuture(view);
                                        return delivery.discard(response)
                                                .thenCompose(
                                                        discarded ->
                                                                CompletableFuture
                                                                        .<PayerHttpResponse>
                                                                                failedFuture(
                                                                                        unwrap(
                                                                                                error)));
                                    })
                            .thenCompose(value -> value)
                            .thenCompose(
                                    reportedView -> {
                                        if (reportedView.status() != 503
                                                || attempt == config.deliveryRetries()) {
                                            return CompletableFuture.completedFuture(response);
                                        }
                                        return delivery.discard(response)
                                                .thenCompose(
                                                        ignored ->
                                                                deliverAsync(
                                                                        context,
                                                                        authorized,
                                                                        delivery,
                                                                        reporter,
                                                                        attempt + 1));
                                    });
                });
    }

    private static Throwable unwrap(Throwable error) {
        return error instanceof java.util.concurrent.CompletionException && error.getCause() != null
                ? error.getCause()
                : error;
    }

    private static void deliveryEvent(
            PayerRequestContext context, AuthorizedPayment authorized, int attemptNo) {
        LOG.log(
                System.Logger.Level.INFO,
                "agentvault.sdk.payer.retrying run_id={0} intent_id={1} attempt_no={2}",
                context.runId(),
                authorized.intent().id(),
                attemptNo);
    }

    private AuthorizePaymentResponse authorizeUntilTerminal(AuthorizePaymentRequest request) {
        long timeoutNanos = config.approvalTimeout().toNanos();
        long started = System.nanoTime();
        String expectedIntent = null;
        for (; ; ) {
            AuthorizePaymentResponse response =
                    transport.post(
                            "/api/v1/payments/authorize", request, AuthorizePaymentResponse.class);
            if (response.authorization() == null || response.intent() == null) {
                throw new IllegalStateException("invalid AgentVault authorization response");
            }
            AuthorizationOutcome outcome = response.authorization().outcome();
            String intentId = response.authorization().intentId();
            if (outcome == null
                    || intentId == null
                    || intentId.isEmpty()
                    || !intentId.equals(response.intent().id())) {
                throw new IllegalStateException("invalid AgentVault authorization response");
            }
            if (expectedIntent == null) expectedIntent = intentId;
            else if (!expectedIntent.equals(intentId)) {
                throw new IllegalStateException("replayed run changed payment intent");
            }
            if (outcome != AuthorizationOutcome.REQUIRES_APPROVAL) return response;
            long remaining = timeoutNanos - (System.nanoTime() - started);
            if (remaining <= 0) {
                throw new IllegalStateException(
                        "payment approval did not complete before the total timeout");
            }
            try {
                TimeUnit.NANOSECONDS.sleep(
                        Math.min(config.approvalPollInterval().toNanos(), remaining));
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
                "/api/v1/payments/" + escape(intentId) + "?scenario_id=" + escape(scenarioId),
                PaymentIntent.class);
    }

    public PaymentReceipt getReceipt(String scenarioId, String intentId) {
        Base.required("scenarioId", scenarioId);
        Base.required("intentId", intentId);
        return transport.get(
                "/api/v1/payments/"
                        + escape(intentId)
                        + "/receipt?scenario_id="
                        + escape(scenarioId),
                PaymentReceipt.class);
    }

    public PaymentBalance getBalance(BalanceQuery query) {
        String path =
                "/api/v1/payments/balance?scenario_id="
                        + escape(query.scenarioId())
                        + "&wallet_id="
                        + escape(query.walletId())
                        + "&network="
                        + escape(query.asset().network())
                        + "&asset="
                        + escape(query.asset().asset())
                        + "&decimals="
                        + query.asset().decimals();
        if (query.asset().tokenContract() != null) {
            path += "&token_contract=" + escape(query.asset().tokenContract());
        }
        return transport.get(path, PaymentBalance.class);
    }

    private ReportOutboundResponseResponse reportOutboundResponse(
            OperationContext context,
            String intentId,
            String protocol,
            io.agentvault.sdk.protocol.PaymentProtocol.MerchantEvidence evidence) {
        Base.required("intent_id", intentId);
        var request =
                new ReportOutboundResponseRequest(
                        context.scenarioId(),
                        context.runId(),
                        protocol,
                        new ProtocolEvidence(evidence.headerName(), evidence.headerValue()));
        return transport.post(
                "/api/v1/payments/" + escape(intentId) + "/outbound-response",
                request,
                ReportOutboundResponseResponse.class);
    }

    static URI safeUri(String value) {
        URI uri = URI.create(value);
        if (!("http".equals(uri.getScheme()) || "https".equals(uri.getScheme()))
                || uri.getHost() == null
                || uri.getUserInfo() != null) {
            throw new IllegalArgumentException(
                    "paid request URL must be absolute HTTP(S) without userinfo");
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
            return StandardCharsets.UTF_8
                    .newDecoder()
                    .onMalformedInput(CodingErrorAction.REPORT)
                    .onUnmappableCharacter(CodingErrorAction.REPORT)
                    .decode(ByteBuffer.wrap(value))
                    .toString();
        } catch (java.nio.charset.CharacterCodingException error) {
            throw new IllegalStateException("payment challenge must be UTF-8", error);
        }
    }
}
