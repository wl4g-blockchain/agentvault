package io.agentvault.sdk.payer;

import com.fasterxml.jackson.core.JsonProcessingException;
import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import io.agentvault.sdk.model.Payer.PayerRequestContext;
import io.agentvault.sdk.model.Payer.PayerHttpRequest;
import io.agentvault.sdk.model.Payer.PayerHttpResponse;
import java.io.IOException;
import java.io.ByteArrayInputStream;
import java.net.URI;
import java.net.http.HttpClient;
import java.util.Optional;
import java.util.concurrent.atomic.AtomicReference;
import org.reactivestreams.Publisher;
import org.springframework.core.io.buffer.DataBuffer;
import org.springframework.core.io.buffer.DataBufferUtils;
import org.springframework.core.io.buffer.DefaultDataBufferFactory;
import org.springframework.http.HttpHeaders;
import org.springframework.http.HttpRequest;
import org.springframework.http.client.ClientHttpRequestExecution;
import org.springframework.http.client.ClientHttpRequestInterceptor;
import org.springframework.http.client.ClientHttpResponse;
import org.springframework.http.client.JdkClientHttpRequestFactory;
import org.springframework.http.client.reactive.ClientHttpRequest;
import org.springframework.http.client.reactive.ClientHttpRequestDecorator;
import org.springframework.web.client.RestClient;
import org.springframework.web.reactive.function.BodyInserter;
import org.springframework.web.reactive.function.BodyInserters;
import org.springframework.web.reactive.function.BodyExtractors;
import org.springframework.web.reactive.function.client.ClientRequest;
import org.springframework.web.reactive.function.client.ClientResponse;
import org.springframework.web.reactive.function.client.ExchangeFilterFunction;
import org.springframework.web.reactive.function.client.ExchangeFunction;
import reactor.core.publisher.Mono;
import reactor.core.publisher.Flux;
import reactor.core.scheduler.Schedulers;

/** Spring RestClient/RestTemplate payer adapter with transparent paid retry. */
public final class SpringRestPayerMiddleware implements ClientHttpRequestInterceptor {
    private static final System.Logger LOG = System.getLogger("io.agentvault.sdk");
    private static final ObjectMapper JSON = new ObjectMapper();
    private record McpCall(JsonNode id, ObjectNode message) {}
    private record BufferedWebResponse(ClientResponse response, byte[] body) {
        private BufferedWebResponse {
            body = body.clone();
        }
        @Override public byte[] body() { return body.clone(); }
    }
    @FunctionalInterface public interface ContextResolver {
        Optional<PayerRequestContext> resolve(HttpRequest request, byte[] body);
    }

    private final PayerMiddleware core;
    private final ContextResolver resolver;

    public SpringRestPayerMiddleware(PayerMiddleware core) {
        this(core, SpringRestPayerMiddleware::contextFromHeaders);
    }

    public SpringRestPayerMiddleware(PayerMiddleware core, ContextResolver resolver) {
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
        AtomicReference<byte[]> captured = new AtomicReference<>();
        ClientRequest firstRequest = captureBody(request, captured);
        return next.exchange(firstRequest).flatMap(first -> {
            byte[] requestBody = captured.get();
            if (requestBody == null) {
                return Mono.error(new IllegalStateException("paid request body was not captured"));
            }
            McpCall call = mcpCall(
                    request.method().name(), PayerMiddleware.copyHeaders(request.headers()), requestBody);
            if (first.statusCode().value() != 402) {
                return handleMcpWebFlux(
                        request, next, target, context.orElseThrow(), call, requestBody, first);
            }
            event("agentvault.sdk.payer.payment_required", context.orElseThrow().runId(), null, null);
            return bufferWeb(first).flatMap(initial -> {
                byte[] challenge = initial.body();
                event("agentvault.sdk.payer.signing_requested", context.orElseThrow().runId(), null, null);
                return Mono.fromCallable(() -> core.authorize(
                                new PayerHttpRequest(
                                        request.method().name(), target.toString(), PayerMiddleware.origin(target),
                                        PayerMiddleware.copyHeaders(request.headers()), requestBody, false),
                                new PayerHttpResponse(
                                        402, PayerMiddleware.copyHeaders(initial.response().headers().asHttpHeaders()),
                                        challenge),
                                context.orElseThrow()))
                        .subscribeOn(Schedulers.boundedElastic())
                        .flatMap(authorized -> {
                            event("agentvault.sdk.payer.signed", context.orElseThrow().runId(),
                                    authorized.intent().id(), null);
                            var delivery = core.credentialHeader(
                                    authorized, PayerMiddleware.origin(target),
                                    PayerMiddleware.copyHeaders(request.headers()));
                            ClientRequest paid = ClientRequest.from(request)
                                    .headers(headers -> {
                                        headers.remove(HttpHeaders.CONTENT_LENGTH);
                                        headers.add(delivery.getKey(), delivery.getValue());
                                    })
                                    .body(BodyInserters.fromValue(requestBody))
                                    .build();
                            return Mono.fromCompletionStage(core.deliverAsync(
                                    context.orElseThrow(), authorized,
                                    new PayerMiddleware.AsyncDelivery<ClientResponse>() {
                                        @Override public java.util.concurrent.CompletionStage<ClientResponse> send(
                                                int attemptNo) {
                                            return next.exchange(paid).toFuture();
                                        }

                                        @Override public PayerHttpResponse view(ClientResponse response) {
                                            return new PayerHttpResponse(
                                                    response.statusCode().value(),
                                                    PayerMiddleware.copyHeaders(response.headers().asHttpHeaders()),
                                                    new byte[0]);
                                        }

                                        @Override public java.util.concurrent.CompletionStage<Void> discard(
                                                ClientResponse response) {
                                            return response.releaseBody().toFuture();
                                        }
                                    }))
                                    .map(response -> response.mutate()
                                            .header("x-agentvault-payment-intent-id", authorized.intent().id())
                                            .build());
                        });
            });
        });
    }

    private Mono<ClientResponse> handleMcpWebFlux(
            ClientRequest request,
            ExchangeFunction next,
            URI target,
            PayerRequestContext context,
            McpCall call,
            byte[] requestBody,
            ClientResponse first) {
        if (call == null || !isJson(first.headers().asHttpHeaders().getFirst("content-type"))) {
            return Mono.just(first);
        }
        return bufferWeb(first).flatMap(initial -> {
            final JsonNode message;
            try {
                message = JSON.readTree(initial.body());
            } catch (IOException error) {
                return Mono.just(rebuild(initial));
            }
            if (!PayerMiddleware.boundMcpResponse(message, call.id(), false)) {
                return Mono.just(rebuild(initial));
            }
            return Mono.fromCallable(() -> core.authorizeMcp(
                            new PayerHttpRequest(
                                    request.method().name(), target.toString(), PayerMiddleware.origin(target),
                                    PayerMiddleware.copyHeaders(request.headers()), requestBody, false),
                            message,
                            context,
                            PayerMiddleware.copyHeaders(initial.response().headers().asHttpHeaders())))
                    .subscribeOn(Schedulers.boundedElastic())
                    .flatMap(authorized -> {
                        if (authorized == null) return Mono.just(rebuild(initial));
                        event("agentvault.sdk.payer.payment_required", context.runId(), null, null);
                        event("agentvault.sdk.payer.signed", context.runId(), authorized.intent().id(), null);
                        var metadata = core.mcpCredential(authorized, PayerMiddleware.origin(target));
                        byte[] paidBody = attachMcpCredential(call, metadata.key(), metadata.value());
                        ClientRequest paid = ClientRequest.from(request)
                                .headers(headers -> headers.remove(HttpHeaders.CONTENT_LENGTH))
                                .body(BodyInserters.fromValue(paidBody))
                                .build();
                        return Mono.fromCompletionStage(core.deliverAsync(
                                        context,
                                        authorized,
                                        new PayerMiddleware.AsyncDelivery<BufferedWebResponse>() {
                                            @Override public java.util.concurrent.CompletionStage<BufferedWebResponse> send(
                                                    int attemptNo) {
                                                return next.exchange(paid)
                                                        .flatMap(SpringRestPayerMiddleware.this::bufferWeb)
                                                        .toFuture();
                                            }

                                            @Override public PayerHttpResponse view(BufferedWebResponse response) {
                                                return new PayerHttpResponse(
                                                        response.response().statusCode().value(),
                                                        PayerMiddleware.copyHeaders(
                                                                response.response().headers().asHttpHeaders()),
                                                        new byte[0]);
                                            }

                                            @Override public java.util.concurrent.CompletionStage<Void> discard(
                                                    BufferedWebResponse ignored) {
                                                return java.util.concurrent.CompletableFuture.completedFuture(null);
                                            }
                                        },
                                        response -> Mono.fromRunnable(() -> reportMcp(
                                                        context, authorized, call, response))
                                                .subscribeOn(Schedulers.boundedElastic())
                                                .then().toFuture()))
                                .map(response -> rebuild(response, authorized.intent().id()));
                    });
        });
    }

    private ClientRequest captureBody(ClientRequest request, AtomicReference<byte[]> captured) {
        @SuppressWarnings("unchecked")
        BodyInserter<Object, ClientHttpRequest> source =
                (BodyInserter<Object, ClientHttpRequest>) request.body();
        BodyInserter<Void, ClientHttpRequest> body = (output, context) -> source.insert(
                new ClientHttpRequestDecorator(output) {
                    @Override public Mono<Void> writeWith(Publisher<? extends DataBuffer> publisher) {
                        return DataBufferUtils.join(publisher, core.config().maxChallengeBytes() + 1)
                                .flatMap(buffer -> {
                                    byte[] value = new byte[buffer.readableByteCount()];
                                    buffer.read(value);
                                    DataBufferUtils.release(buffer);
                                    if (value.length > core.config().maxChallengeBytes()) {
                                        return Mono.error(new IllegalArgumentException(
                                                "paid request body exceeds configured limit"));
                                    }
                                    captured.set(value);
                                    return super.writeWith(Mono.just(output.bufferFactory().wrap(value)));
                                });
                    }

                    @Override public Mono<Void> writeAndFlushWith(
                            Publisher<? extends Publisher<? extends DataBuffer>> body) {
                        return Mono.error(new IllegalArgumentException(
                                "streaming paid requests cannot be replayed"));
                    }

                    @Override public Mono<Void> setComplete() {
                        captured.compareAndSet(null, new byte[0]);
                        return super.setComplete();
                    }
                }, context);
        return ClientRequest.from(request).body(body).build();
    }

    private Mono<BufferedWebResponse> bufferWeb(ClientResponse response) {
        return DataBufferUtils.join(
                        response.body(BodyExtractors.toDataBuffers()),
                        core.config().maxChallengeBytes() + 1)
                .map(buffer -> {
                    byte[] body = new byte[buffer.readableByteCount()];
                    buffer.read(body);
                    DataBufferUtils.release(buffer);
                    if (body.length > core.config().maxChallengeBytes()) {
                        throw new IllegalArgumentException("payment response exceeds configured limit");
                    }
                    return new BufferedWebResponse(response, body);
                })
                .defaultIfEmpty(new BufferedWebResponse(response, new byte[0]));
    }

    private ClientResponse rebuild(BufferedWebResponse response) {
        return rebuild(response, null);
    }

    private ClientResponse rebuild(BufferedWebResponse response, String intentId) {
        ClientResponse original = response.response();
        ClientResponse.Builder builder = ClientResponse.create(
                        original.statusCode(), original.strategies())
                .headers(headers -> headers.putAll(original.headers().asHttpHeaders()))
                .cookies(cookies -> cookies.putAll(original.cookies()))
                .request(original.request())
                .body(Flux.just(DefaultDataBufferFactory.sharedInstance.wrap(response.body())));
        if (intentId != null) {
            builder.header("x-agentvault-payment-intent-id", intentId);
        }
        return builder.build();
    }

    private void reportMcp(
            PayerRequestContext context,
            io.agentvault.sdk.model.Payer.AuthorizedPayment authorized,
            McpCall call,
            BufferedWebResponse response) {
        int status = response.response().statusCode().value();
        if (status == 503 || status < 200 || status >= 300) return;
        if (!isJson(response.response().headers().asHttpHeaders().getFirst("content-type"))) {
            throw new IllegalStateException("paid MCP response must be application/json");
        }
        final JsonNode message;
        try {
            message = JSON.readTree(response.body());
        } catch (IOException error) {
            throw new IllegalStateException("paid MCP response is not valid JSON", error);
        }
        if (!PayerMiddleware.boundMcpResponse(message, call.id(), true)) {
            throw new IllegalStateException("paid MCP response has no matching JSON-RPC result");
        }
        JsonNode metadata = message.path("result").path("_meta");
        core.reportMcp(context, authorized,
                metadata.isObject() ? metadata : JSON.createObjectNode());
    }

    @Override public ClientHttpResponse intercept(
            HttpRequest request,
            byte[] body,
            ClientHttpRequestExecution execution) throws IOException {
        Optional<PayerRequestContext> context = resolver.resolve(request, body);
        if (context.isEmpty()) return execution.execute(request, body);
        if (body.length > core.config().maxChallengeBytes()) {
            throw new IOException("paid request body exceeds configured limit");
        }
        URI target = PayerMiddleware.safeUri(request.getURI().toString());
        McpCall call = mcpCall(
                request.getMethod().name(), PayerMiddleware.copyHeaders(request.getHeaders()), body);
        ClientHttpResponse first = execution.execute(request, body);
        if (first.getStatusCode().value() != 402) {
            ClientHttpResponse paid = handleMcpChallenge(
                    request, body, execution, context.get(), target, call, first);
            return paid == null ? first : paid;
        }
        event("agentvault.sdk.payer.payment_required", context.get().runId(), null, null);
        byte[] challenge;
        try (first) {
            challenge = first.getBody().readNBytes(core.config().maxChallengeBytes() + 1);
        }
        if (challenge.length > core.config().maxChallengeBytes()) {
            throw new IOException("payment challenge exceeds configured limit");
        }
        event("agentvault.sdk.payer.signing_requested", context.get().runId(), null, null);
        var authorized = core.authorize(
                new PayerHttpRequest(
                        request.getMethod().name(), target.toString(), PayerMiddleware.origin(target),
                        PayerMiddleware.copyHeaders(request.getHeaders()), body, false),
                new PayerHttpResponse(
                        402, PayerMiddleware.copyHeaders(first.getHeaders()), challenge),
                context.get());
        event("agentvault.sdk.payer.signed", context.get().runId(), authorized.intent().id(), null);
        var delivery = core.credentialHeader(
                authorized, PayerMiddleware.origin(target), PayerMiddleware.copyHeaders(request.getHeaders()));
        HttpHeaders headers = new HttpHeaders();
        headers.putAll(request.getHeaders());
        headers.add(delivery.getKey(), delivery.getValue());
        HttpRequest paid = new HeaderRequest(request, headers);
        ClientHttpResponse response = core.deliver(
                context.get(), authorized,
                ignored -> execution.execute(paid, body),
                new PayerMiddleware.DeliveryResponse<ClientHttpResponse, IOException>() {
                    @Override public PayerHttpResponse view(ClientHttpResponse value) throws IOException {
                        return new PayerHttpResponse(
                                value.getStatusCode().value(),
                                PayerMiddleware.copyHeaders(value.getHeaders()), new byte[0]);
                    }

                    @Override public void discard(ClientHttpResponse value) {
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
            McpCall call,
            ClientHttpResponse first) throws IOException {
        if (call == null || !isJson(first.getHeaders().getFirst("content-type"))) return null;
        BufferedResponse initial = buffer(first);
        final JsonNode message;
        try {
            message = JSON.readTree(initial.body());
        } catch (JsonProcessingException error) {
            return initial;
        }
        if (!PayerMiddleware.boundMcpResponse(message, call.id(), false)) return initial;
        var authorized = core.authorizeMcp(
                new PayerHttpRequest(
                        request.getMethod().name(), target.toString(), PayerMiddleware.origin(target),
                        PayerMiddleware.copyHeaders(request.getHeaders()), requestBody, false),
                message,
                context,
                PayerMiddleware.copyHeaders(initial.getHeaders()));
        if (authorized == null) return initial;
        event("agentvault.sdk.payer.payment_required", context.runId(), null, null);
        event("agentvault.sdk.payer.signed", context.runId(), authorized.intent().id(), null);
        var metadata = core.mcpCredential(authorized, PayerMiddleware.origin(target));
        byte[] paidBody = attachMcpCredential(call, metadata.key(), metadata.value());
        HttpHeaders paidHeaders = new HttpHeaders();
        paidHeaders.putAll(request.getHeaders());
        paidHeaders.remove(HttpHeaders.CONTENT_LENGTH);
        HttpRequest paidRequest = new HeaderRequest(request, paidHeaders);
        BufferedResponse response = core.deliver(
                context,
                authorized,
                ignored -> buffer(execution.execute(paidRequest, paidBody)),
                new PayerMiddleware.DeliveryResponse<BufferedResponse, IOException>() {
                    @Override public PayerHttpResponse view(BufferedResponse value) {
                        return new PayerHttpResponse(
                                value.getStatusCode().value(),
                                PayerMiddleware.copyHeaders(value.getHeaders()), new byte[0]);
                    }

                    @Override public void discard(BufferedResponse ignored) {
                        // Buffered responses have no live stream to release.
                    }
                },
                value -> reportMcp(context, authorized, call, value));
        return new MetadataResponse(response, authorized.intent().id());
    }

    private void reportMcp(
            PayerRequestContext context,
            io.agentvault.sdk.model.Payer.AuthorizedPayment authorized,
            McpCall call,
            BufferedResponse response) throws IOException {
        int status = response.getStatusCode().value();
        if (status == 503 || status < 200 || status >= 300) return;
        if (!isJson(response.getHeaders().getFirst("content-type"))) {
            throw new IOException("paid MCP response must be application/json");
        }
        final JsonNode message;
        try {
            message = JSON.readTree(response.body());
        } catch (JsonProcessingException error) {
            throw new IOException("paid MCP response is not valid JSON", error);
        }
        if (!PayerMiddleware.boundMcpResponse(message, call.id(), true)) {
            throw new IOException("paid MCP response has no matching JSON-RPC result");
        }
        JsonNode metadata = message.path("result").path("_meta");
        core.reportMcp(context, authorized,
                metadata.isObject() ? metadata : JSON.createObjectNode());
    }

    private BufferedResponse buffer(ClientHttpResponse response) throws IOException {
        try (response) {
            byte[] body = response.getBody().readNBytes(core.config().maxChallengeBytes() + 1);
            if (body.length > core.config().maxChallengeBytes()) {
                throw new IOException("payment response exceeds configured limit");
            }
            return new BufferedResponse(
                    response.getStatusCode(), response.getStatusText(), response.getHeaders(), body);
        }
    }

    private McpCall mcpCall(String method, java.util.Map<String, String> headers, byte[] body) {
        if (!"POST".equalsIgnoreCase(method)
                || !isJson(PayerMiddleware.header(headers, "content-type"))) return null;
        try {
            JsonNode value = JSON.readTree(body);
            if (value == null || !value.isObject() || !"2.0".equals(value.path("jsonrpc").asText())
                    || !"tools/call".equals(value.path("method").asText())
                    || !value.path("params").isObject()
                    || !(value.path("id").isTextual() || value.path("id").isNumber())) return null;
            return new McpCall(value.get("id"), (ObjectNode) value);
        } catch (IOException error) {
            return null;
        }
    }

    private byte[] attachMcpCredential(McpCall call, String key, JsonNode value) {
        ObjectNode message = call.message().deepCopy();
        ObjectNode params = (ObjectNode) message.path("params");
        JsonNode rootExisting = message.get("_meta");
        if (rootExisting != null && !rootExisting.isObject()) {
            throw new IllegalArgumentException("JSON-RPC root _meta must be an object");
        }
        if (rootExisting != null && core.mcpRootRequestMetadataKeys().stream().anyMatch(rootExisting::has)) {
            throw new IllegalArgumentException("MCP request already contains root payment credentials");
        }
        JsonNode existing = params.get("_meta");
        if (existing != null && !existing.isObject()) {
            throw new IllegalArgumentException("MCP params._meta must be an object");
        }
        ObjectNode metadata = existing == null
                ? JSON.createObjectNode() : ((ObjectNode) existing).deepCopy();
        if (core.mcpRequestMetadataKeys().stream().anyMatch(metadata::has)) {
            throw new IllegalArgumentException("MCP request already contains payment credentials");
        }
        metadata.set(key, value);
        params.set("_meta", metadata);
        try {
            byte[] encoded = JSON.writeValueAsBytes(message);
            if (encoded.length > core.config().maxChallengeBytes()) {
                throw new IllegalArgumentException("paid MCP request exceeds configured limit");
            }
            return encoded;
        } catch (JsonProcessingException error) {
            throw new IllegalArgumentException("paid MCP request cannot be encoded", error);
        }
    }

    private static boolean isJson(String value) {
        return value != null
                && value.toLowerCase(java.util.Locale.ROOT).startsWith("application/json");
    }

    private static void event(String event, String runId, String intentId, Integer attemptNo) {
        LOG.log(System.Logger.Level.INFO, "{0} run_id={1} intent_id={2} attempt_no={3}",
                event, runId, intentId == null ? "" : intentId, attemptNo == null ? "" : attemptNo);
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

        private byte[] body() { return body.clone(); }
        @Override public org.springframework.http.HttpStatusCode getStatusCode() { return status; }
        @Override public String getStatusText() { return statusText; }
        @Override public HttpHeaders getHeaders() { return headers; }
        @Override public java.io.InputStream getBody() { return new ByteArrayInputStream(body); }
        @Override public void close() {}
    }
}
