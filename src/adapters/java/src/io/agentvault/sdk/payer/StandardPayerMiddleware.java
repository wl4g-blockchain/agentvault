package io.agentvault.sdk.payer;

import io.agentvault.sdk.model.Payer.PayerRequestContext;
import io.agentvault.sdk.model.Payer.PayerHttpRequest;
import io.agentvault.sdk.model.Payer.PayerHttpResponse;
import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.util.Optional;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpRequest;
import org.springframework.http.client.ClientHttpRequestExecution;
import org.springframework.http.client.ClientHttpRequestInterceptor;
import org.springframework.http.client.ClientHttpResponse;
import org.springframework.http.client.JdkClientHttpRequestFactory;
import org.springframework.web.client.RestClient;
import org.springframework.web.reactive.function.client.ClientRequest;
import org.springframework.web.reactive.function.client.ClientResponse;
import org.springframework.web.reactive.function.client.ExchangeFilterFunction;
import org.springframework.web.reactive.function.client.ExchangeFunction;
import reactor.core.publisher.Mono;
import reactor.core.scheduler.Schedulers;

/** Spring RestClient/RestTemplate payer adapter with transparent paid retry. */
public final class StandardPayerMiddleware implements ClientHttpRequestInterceptor {
    @FunctionalInterface public interface ContextResolver {
        Optional<PayerRequestContext> resolve(HttpRequest request, byte[] body);
    }

    private final PayerMiddleware core;
    private final ContextResolver resolver;

    public StandardPayerMiddleware(PayerMiddleware core) {
        this(core, StandardPayerMiddleware::contextFromHeaders);
    }

    public StandardPayerMiddleware(PayerMiddleware core, ContextResolver resolver) {
        this.core = java.util.Objects.requireNonNull(core, "core");
        this.resolver = java.util.Objects.requireNonNull(resolver, "resolver");
    }

    /**
     * Builds the safe default Spring client. Redirects are disabled at the
     * underlying JDK client so a payment credential can never be forwarded to
     * another authority before this interceptor sees the response.
     */
    public RestClient client() {
        HttpClient client = HttpClient.newBuilder()
                .followRedirects(HttpClient.Redirect.NEVER)
                .build();
        return RestClient.builder()
                .requestFactory(new JdkClientHttpRequestFactory(client))
                .requestInterceptor(this)
                .build();
    }

    /**
     * Returns a WebClient filter for reactive frameworks and MCP transports.
     * The filter owns the complete 402 state machine; callers only supply the
     * stable run header on protected operations.
     */
    public ExchangeFilterFunction webClientFilter() {
        return this::filter;
    }

    private Mono<ClientResponse> filter(ClientRequest request, ExchangeFunction next) {
        Optional<PayerRequestContext> context = contextFromHeaders(request);
        if (context.isEmpty()) return next.exchange(request);
        URI target;
        try {
            target = PayerMiddleware.safeUri(request.url().toString());
        } catch (RuntimeException error) {
            return Mono.error(error);
        }
        return next.exchange(request).flatMap(first -> {
            if (first.statusCode().value() != 402) return Mono.just(first);
            return first.bodyToMono(byte[].class).defaultIfEmpty(new byte[0]).flatMap(challenge -> {
                if (challenge.length > core.config().maxChallengeBytes()) {
                    return Mono.error(new IOException("payment challenge exceeds configured limit"));
                }
                return Mono.fromCallable(() -> core.authorize(
                                new PayerHttpRequest(
                                        request.method().name(), target.toString(), PayerMiddleware.origin(target),
                                        PayerMiddleware.copyHeaders(request.headers()), new byte[0], false),
                                new PayerHttpResponse(
                                        402, PayerMiddleware.copyHeaders(first.headers().asHttpHeaders()), challenge),
                                context.orElseThrow()))
                        .subscribeOn(Schedulers.boundedElastic())
                        .flatMap(authorized -> {
                            var delivery = core.credentialHeader(
                                    authorized, PayerMiddleware.origin(target),
                                    PayerMiddleware.copyHeaders(request.headers()));
                            ClientRequest paid = ClientRequest.from(request)
                                    .headers(headers -> headers.add(delivery.getKey(), delivery.getValue()))
                                    .build();
                            return deliver(next, paid, context.orElseThrow(), authorized, 0);
                        });
            });
        });
    }

    private Mono<ClientResponse> deliver(
            ExchangeFunction next,
            ClientRequest request,
            PayerRequestContext context,
            io.agentvault.sdk.model.Payer.AuthorizedPayment authorized,
            int attempt) {
        return next.exchange(request).flatMap(response -> Mono.fromRunnable(() -> core.report(
                        context,
                        authorized,
                        new PayerHttpResponse(
                                response.statusCode().value(),
                                PayerMiddleware.copyHeaders(response.headers().asHttpHeaders()),
                                new byte[0])))
                .subscribeOn(Schedulers.boundedElastic())
                .then(Mono.defer(() -> {
                    if (response.statusCode().value() == 503 && attempt < core.config().deliveryRetries()) {
                        return response.releaseBody().then(deliver(next, request, context, authorized, attempt + 1));
                    }
                    return Mono.just(response.mutate()
                            .header("x-agentvault-payment-intent-id", authorized.intent().id())
                            .build());
                })));
    }

    @Override public ClientHttpResponse intercept(
            HttpRequest request,
            byte[] body,
            ClientHttpRequestExecution execution) throws IOException {
        Optional<PayerRequestContext> context = resolver.resolve(request, body);
        if (context.isEmpty()) return execution.execute(request, body);
        URI target = PayerMiddleware.safeUri(request.getURI().toString());
        ClientHttpResponse first = execution.execute(request, body);
        if (first.getStatusCode().value() != 402) return first;
        byte[] challenge;
        try (first) {
            challenge = first.getBody().readNBytes(core.config().maxChallengeBytes() + 1);
        }
        if (challenge.length > core.config().maxChallengeBytes()) {
            throw new IOException("payment challenge exceeds configured limit");
        }
        var authorized = core.authorize(
                new PayerHttpRequest(
                        request.getMethod().name(), target.toString(), PayerMiddleware.origin(target),
                        PayerMiddleware.copyHeaders(request.getHeaders()), body, false),
                new PayerHttpResponse(
                        402, PayerMiddleware.copyHeaders(first.getHeaders()), challenge),
                context.get());
        var delivery = core.credentialHeader(
                authorized, PayerMiddleware.origin(target), PayerMiddleware.copyHeaders(request.getHeaders()));
        HttpHeaders headers = new HttpHeaders();
        headers.putAll(request.getHeaders());
        headers.add(delivery.getKey(), delivery.getValue());
        HttpRequest paid = new HeaderRequest(request, headers);
        for (int attempt = 0; attempt <= core.config().deliveryRetries(); attempt++) {
            ClientHttpResponse response = execution.execute(paid, body);
            core.report(context.get(), authorized, new PayerHttpResponse(
                    response.getStatusCode().value(), PayerMiddleware.copyHeaders(response.getHeaders()), new byte[0]));
            if (response.getStatusCode().value() != 503 || attempt == core.config().deliveryRetries()) {
                return new MetadataResponse(response, authorized.intent().id());
            }
            response.close();
        }
        throw new IOException("payer retry loop ended unexpectedly");
    }

    private static Optional<PayerRequestContext> contextFromHeaders(HttpRequest request, byte[] ignored) {
        String runId = request.getHeaders().getFirst("x-run-id");
        return runId == null ? Optional.empty() : Optional.of(new PayerRequestContext(runId));
    }

    private static Optional<PayerRequestContext> contextFromHeaders(ClientRequest request) {
        String runId = request.headers().getFirst("x-run-id");
        return runId == null ? Optional.empty() : Optional.of(new PayerRequestContext(runId));
    }

    private record HeaderRequest(HttpRequest delegate, HttpHeaders headers) implements HttpRequest {
        @Override public org.springframework.http.HttpMethod getMethod() { return delegate.getMethod(); }
        @Override public URI getURI() { return delegate.getURI(); }
        @Override public HttpHeaders getHeaders() { return headers; }
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

        @Override public org.springframework.http.HttpStatusCode getStatusCode() throws IOException {
            return delegate.getStatusCode();
        }
        @Override public String getStatusText() throws IOException { return delegate.getStatusText(); }
        @Override public HttpHeaders getHeaders() { return headers; }
        @Override public java.io.InputStream getBody() throws IOException { return delegate.getBody(); }
        @Override public void close() { delegate.close(); }
    }
}
