package io.agentvault.sdk.payer;

import static io.agentvault.sdk.payer.PayerMessages.JSON;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.JsonNode;

import io.agentvault.sdk.model.Payer.PayerHttpRequest;
import io.agentvault.sdk.model.Payer.PayerHttpResponse;
import io.agentvault.sdk.model.Payer.PayerRequestContext;
import io.agentvault.sdk.payer.PayerMessages.Call;

import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpRequest;
import org.springframework.http.client.ClientHttpRequestExecution;
import org.springframework.http.client.ClientHttpRequestInterceptor;
import org.springframework.http.client.ClientHttpResponse;
import org.springframework.http.client.JdkClientHttpRequestFactory;
import org.springframework.web.client.RestClient;
import org.springframework.web.reactive.function.client.ExchangeFilterFunction;

import java.io.ByteArrayInputStream;
import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.util.Optional;

/** Spring RestClient/RestTemplate payer adapter with transparent paid retry. */
public final class SpringRestPayerMiddleware implements ClientHttpRequestInterceptor {

    @FunctionalInterface
    public interface ContextResolver {
        Optional<PayerRequestContext> resolve(HttpRequest request, byte[] body);
    }

    private final PayerMiddleware core;
    private final PayerMessages messages;
    private final ContextResolver resolver;

    public SpringRestPayerMiddleware(PayerMiddleware core) {
        this(core, SpringRestPayerMiddleware::contextFromHeaders);
    }

    public SpringRestPayerMiddleware(PayerMiddleware core, ContextResolver resolver) {
        this.core = java.util.Objects.requireNonNull(core, "core");
        this.messages = new PayerMessages(core);
        this.resolver = java.util.Objects.requireNonNull(resolver, "resolver");
    }

    /**
     * Builds the safe default Spring client. Redirects are disabled at the underlying JDK client so
     * a payment credential can never be forwarded to another authority before this interceptor sees
     * the response.
     */
    public RestClient client() {
        HttpClient client =
                HttpClient.newBuilder().followRedirects(HttpClient.Redirect.NEVER).build();
        return RestClient.builder()
                .requestFactory(new JdkClientHttpRequestFactory(client))
                .requestInterceptor(this)
                .build();
    }

    /**
     * Returns the reactive adapter over the same payer orchestration core. It captures replayable
     * request bodies and converts responses; callers supply the stable run header.
     */
    public ExchangeFilterFunction webClientFilter() {
        return new SpringWebFluxPayerMiddleware(core).filter();
    }

    @Override
    public ClientHttpResponse intercept(
            HttpRequest request, byte[] body, ClientHttpRequestExecution execution)
            throws IOException {
        Optional<PayerRequestContext> context = resolver.resolve(request, body);
        if (context.isEmpty()) return execution.execute(request, body);
        if (body.length > core.config().maxChallengeBytes()) {
            throw new IOException("paid request body exceeds configured limit");
        }
        URI target = PayerMiddleware.safeUri(request.getURI().toString());
        Call call =
                messages.parse(
                        request.getMethod().name(),
                        PayerMiddleware.copyHeaders(request.getHeaders()),
                        body);
        if (call != null && call.a2a() != null) {
            var adapted = new HttpHeaders();
            adapted.putAll(request.getHeaders());
            adapted.set(
                    "x-a2a-extensions",
                    core.a2a().extensions(adapted.getFirst("x-a2a-extensions")));
            request = new HeaderRequest(request, adapted);
        }
        ClientHttpResponse first = execution.execute(request, body);
        if (first.getStatusCode().value() != 402) {
            ClientHttpResponse paid =
                    handleMcpChallenge(
                            request, body, execution, context.get(), target, call, first);
            return paid == null ? first : paid;
        }
        PayerMessages.event(
                "agentvault.sdk.payer.payment_required", context.get().runId(), null, null);
        byte[] challenge;
        try (first) {
            challenge = first.getBody().readNBytes(core.config().maxChallengeBytes() + 1);
        }
        if (challenge.length > core.config().maxChallengeBytes()) {
            throw new IOException("payment challenge exceeds configured limit");
        }
        PayerMessages.event(
                "agentvault.sdk.payer.signing_requested", context.get().runId(), null, null);
        var authorized =
                core.authorize(
                        new PayerHttpRequest(
                                request.getMethod().name(),
                                target.toString(),
                                PayerMiddleware.origin(target),
                                PayerMiddleware.copyHeaders(request.getHeaders()),
                                body,
                                false),
                        new PayerHttpResponse(
                                402, PayerMiddleware.copyHeaders(first.getHeaders()), challenge),
                        context.get());
        PayerMessages.event(
                "agentvault.sdk.payer.signed",
                context.get().runId(),
                authorized.intent().id(),
                null);
        var delivery =
                core.credentialHeader(
                        authorized,
                        PayerMiddleware.origin(target),
                        PayerMiddleware.copyHeaders(request.getHeaders()));
        HttpHeaders headers = new HttpHeaders();
        headers.putAll(request.getHeaders());
        headers.add(delivery.getKey(), delivery.getValue());
        HttpRequest paid = new HeaderRequest(request, headers);
        ClientHttpResponse response =
                core.deliver(
                        context.get(),
                        authorized,
                        ignored -> execution.execute(paid, body),
                        new PayerMiddleware.DeliveryResponse<ClientHttpResponse, IOException>() {
                            @Override
                            public PayerHttpResponse view(ClientHttpResponse value)
                                    throws IOException {
                                return new PayerHttpResponse(
                                        value.getStatusCode().value(),
                                        PayerMiddleware.copyHeaders(value.getHeaders()),
                                        new byte[0]);
                            }

                            @Override
                            public void discard(ClientHttpResponse value) {
                                value.close();
                            }
                        });
        return new MetadataResponse(response, authorized.intent().id());
    }

    private ClientHttpResponse handleMcpChallenge(
            HttpRequest request,
            byte[] requestBody,
            ClientHttpRequestExecution execution,
            PayerRequestContext context,
            URI target,
            Call call,
            ClientHttpResponse first)
            throws IOException {
        if (call == null || !PayerMessages.isJson(first.getHeaders().getFirst("content-type")))
            return null;
        BufferedResponse initial = buffer(first);
        final JsonNode message;
        try {
            message = JSON.readTree(initial.body());
        } catch (JsonProcessingException error) {
            return initial;
        }
        if (!PayerMiddleware.boundMcpResponse(message, call.id(), false)) return initial;
        var authorized =
                core.authorizeMessage(
                        new PayerHttpRequest(
                                request.getMethod().name(),
                                target.toString(),
                                PayerMiddleware.origin(target),
                                PayerMiddleware.copyHeaders(request.getHeaders()),
                                requestBody,
                                false),
                        message,
                        context,
                        PayerMiddleware.copyHeaders(initial.getHeaders()));
        if (authorized == null) return initial;
        PayerMessages.event("agentvault.sdk.payer.payment_required", context.runId(), null, null);
        PayerMessages.event(
                "agentvault.sdk.payer.signed", context.runId(), authorized.intent().id(), null);
        byte[] paidBody;
        if (call.a2a() != null)
            paidBody =
                    core.a2a()
                            .credential(
                                    authorized,
                                    call.a2a(),
                                    message,
                                    PayerMiddleware.origin(target));
        else {
            var metadata = core.mcpCredential(authorized, PayerMiddleware.origin(target));
            paidBody = messages.credential(call, metadata.key(), metadata.value());
        }
        HttpHeaders paidHeaders = new HttpHeaders();
        paidHeaders.putAll(request.getHeaders());
        paidHeaders.remove(HttpHeaders.CONTENT_LENGTH);
        HttpRequest paidRequest = new HeaderRequest(request, paidHeaders);
        BufferedResponse response =
                core.deliver(
                        context,
                        authorized,
                        ignored -> buffer(execution.execute(paidRequest, paidBody)),
                        new PayerMiddleware.DeliveryResponse<BufferedResponse, IOException>() {
                            @Override
                            public PayerHttpResponse view(BufferedResponse value) {
                                return new PayerHttpResponse(
                                        value.getStatusCode().value(),
                                        PayerMiddleware.copyHeaders(value.getHeaders()),
                                        new byte[0]);
                            }

                            @Override
                            public void discard(BufferedResponse ignored) {
                                // Buffered responses have no live stream to release.
                            }
                        },
                        value -> reportMcp(context, authorized, call, value));
        return new MetadataResponse(response, authorized.intent().id());
    }

    private void reportMcp(
            PayerRequestContext context,
            io.agentvault.sdk.model.Payer.AuthorizedPayment authorized,
            Call call,
            BufferedResponse response)
            throws IOException {
        messages.report(
                context,
                authorized,
                call,
                response.getStatusCode().value(),
                PayerMiddleware.copyHeaders(response.getHeaders()),
                response.body());
    }

    private BufferedResponse buffer(ClientHttpResponse response) throws IOException {
        try (response) {
            byte[] body = response.getBody().readNBytes(core.config().maxChallengeBytes() + 1);
            if (body.length > core.config().maxChallengeBytes()) {
                throw new IOException("payment response exceeds configured limit");
            }
            return new BufferedResponse(
                    response.getStatusCode(),
                    response.getStatusText(),
                    response.getHeaders(),
                    body);
        }
    }

    private static Optional<PayerRequestContext> contextFromHeaders(
            HttpRequest request, byte[] ignored) {
        String runId = request.getHeaders().getFirst("x-run-id");
        return runId == null ? Optional.empty() : Optional.of(new PayerRequestContext(runId));
    }

    private record HeaderRequest(HttpRequest delegate, HttpHeaders headers) implements HttpRequest {
        @Override
        public org.springframework.http.HttpMethod getMethod() {
            return delegate.getMethod();
        }

        @Override
        public URI getURI() {
            return delegate.getURI();
        }

        @Override
        public HttpHeaders getHeaders() {
            return headers;
        }

        @Override
        public java.util.Map<String, Object> getAttributes() {
            return delegate.getAttributes();
        }
    }

    private static final class MetadataResponse implements ClientHttpResponse {
        private final ClientHttpResponse delegate;
        private final HttpHeaders headers;

        private MetadataResponse(ClientHttpResponse delegate, String intentId) {
            this.delegate = delegate;
            this.headers = new HttpHeaders();
            this.headers.putAll(delegate.getHeaders());
            this.headers.set("x-agentvault-payment-intent-id", intentId);
        }

        @Override
        public org.springframework.http.HttpStatusCode getStatusCode() throws IOException {
            return delegate.getStatusCode();
        }

        @Override
        public String getStatusText() throws IOException {
            return delegate.getStatusText();
        }

        @Override
        public HttpHeaders getHeaders() {
            return headers;
        }

        @Override
        public java.io.InputStream getBody() throws IOException {
            return delegate.getBody();
        }

        @Override
        public void close() {
            delegate.close();
        }
    }

    private static final class BufferedResponse implements ClientHttpResponse {
        private final org.springframework.http.HttpStatusCode status;
        private final String statusText;
        private final HttpHeaders headers;
        private final byte[] body;

        private BufferedResponse(
                org.springframework.http.HttpStatusCode status,
                String statusText,
                HttpHeaders headers,
                byte[] body) {
            this.status = status;
            this.statusText = statusText;
            this.headers = new HttpHeaders();
            this.headers.putAll(headers);
            this.body = body.clone();
        }

        private byte[] body() {
            return body.clone();
        }

        @Override
        public org.springframework.http.HttpStatusCode getStatusCode() {
            return status;
        }

        @Override
        public String getStatusText() {
            return statusText;
        }

        @Override
        public HttpHeaders getHeaders() {
            return headers;
        }

        @Override
        public java.io.InputStream getBody() {
            return new ByteArrayInputStream(body);
        }

        @Override
        public void close() {}
    }
}
