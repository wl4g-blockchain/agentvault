package io.agentvault.sdk.payer;

import static io.agentvault.sdk.payer.SpringPayerMessages.JSON;

import com.fasterxml.jackson.databind.JsonNode;

import io.agentvault.sdk.model.A2a;
import io.agentvault.sdk.model.Payer.PayerHttpRequest;
import io.agentvault.sdk.model.Payer.PayerHttpResponse;
import io.agentvault.sdk.model.Payer.PayerRequestContext;
import io.agentvault.sdk.payer.SpringPayerMessages.Call;

import org.reactivestreams.Publisher;
import org.springframework.core.io.buffer.DataBuffer;
import org.springframework.core.io.buffer.DataBufferUtils;
import org.springframework.core.io.buffer.DefaultDataBufferFactory;
import org.springframework.http.HttpHeaders;
import org.springframework.http.client.reactive.ClientHttpRequest;
import org.springframework.http.client.reactive.ClientHttpRequestDecorator;
import org.springframework.web.reactive.function.BodyExtractors;
import org.springframework.web.reactive.function.BodyInserter;
import org.springframework.web.reactive.function.BodyInserters;
import org.springframework.web.reactive.function.client.ClientRequest;
import org.springframework.web.reactive.function.client.ClientResponse;
import org.springframework.web.reactive.function.client.ExchangeFilterFunction;
import org.springframework.web.reactive.function.client.ExchangeFunction;

import reactor.core.publisher.Flux;
import reactor.core.publisher.Mono;
import reactor.core.scheduler.Schedulers;

import java.io.IOException;
import java.net.URI;
import java.util.Optional;
import java.util.concurrent.atomic.AtomicReference;

/** Replayable Spring WebFlux request/response adaptation over PayerMiddleware. */
final class SpringWebFluxPayerMiddleware {
    private final PayerMiddleware core;
    private final SpringPayerMessages messages;

    SpringWebFluxPayerMiddleware(PayerMiddleware core) {
        this.core = java.util.Objects.requireNonNull(core, "core");
        this.messages = new SpringPayerMessages(core);
    }

    ExchangeFilterFunction filter() {
        return this::filter;
    }

    private record BufferedWebResponse(ClientResponse response, byte[] body) {
        private BufferedWebResponse {
            body = body.clone();
        }

        @Override
        public byte[] body() {
            return body.clone();
        }
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
        return next.exchange(firstRequest)
                .flatMap(
                        first -> {
                            byte[] requestBody = captured.get();
                            if (requestBody == null) {
                                return Mono.error(
                                        new IllegalStateException(
                                                "paid request body was not captured"));
                            }
                            Call call =
                                    messages.parse(
                                            request.method().name(),
                                            PayerMiddleware.copyHeaders(request.headers()),
                                            requestBody);
                            if (first.statusCode().value() != 402) {
                                return handleMcpWebFlux(
                                        request,
                                        next,
                                        target,
                                        context.orElseThrow(),
                                        call,
                                        requestBody,
                                        first);
                            }
                            SpringPayerMessages.event(
                                    "agentvault.sdk.payer.payment_required",
                                    context.orElseThrow().runId(),
                                    null,
                                    null);
                            return bufferWeb(first)
                                    .flatMap(
                                            initial -> {
                                                byte[] challenge = initial.body();
                                                SpringPayerMessages.event(
                                                        "agentvault.sdk.payer.signing_requested",
                                                        context.orElseThrow().runId(),
                                                        null,
                                                        null);
                                                return Mono.fromCallable(
                                                                () ->
                                                                        core.authorize(
                                                                                new PayerHttpRequest(
                                                                                        request.method()
                                                                                                .name(),
                                                                                        target
                                                                                                .toString(),
                                                                                        PayerMiddleware
                                                                                                .origin(
                                                                                                        target),
                                                                                        PayerMiddleware
                                                                                                .copyHeaders(
                                                                                                        request
                                                                                                                .headers()),
                                                                                        requestBody,
                                                                                        false),
                                                                                new PayerHttpResponse(
                                                                                        402,
                                                                                        PayerMiddleware
                                                                                                .copyHeaders(
                                                                                                        initial.response()
                                                                                                                .headers()
                                                                                                                .asHttpHeaders()),
                                                                                        challenge),
                                                                                context
                                                                                        .orElseThrow()))
                                                        .subscribeOn(Schedulers.boundedElastic())
                                                        .flatMap(
                                                                authorized -> {
                                                                    SpringPayerMessages.event(
                                                                            "agentvault.sdk.payer.signed",
                                                                            context.orElseThrow()
                                                                                    .runId(),
                                                                            authorized
                                                                                    .intent()
                                                                                    .id(),
                                                                            null);
                                                                    var delivery =
                                                                            core.credentialHeader(
                                                                                    authorized,
                                                                                    PayerMiddleware
                                                                                            .origin(
                                                                                                    target),
                                                                                    PayerMiddleware
                                                                                            .copyHeaders(
                                                                                                    request
                                                                                                            .headers()));
                                                                    ClientRequest paid =
                                                                            ClientRequest.from(
                                                                                            request)
                                                                                    .headers(
                                                                                            headers -> {
                                                                                                headers
                                                                                                        .remove(
                                                                                                                HttpHeaders
                                                                                                                        .CONTENT_LENGTH);
                                                                                                headers
                                                                                                        .add(
                                                                                                                delivery
                                                                                                                        .getKey(),
                                                                                                                delivery
                                                                                                                        .getValue());
                                                                                            })
                                                                                    .body(
                                                                                            BodyInserters
                                                                                                    .fromValue(
                                                                                                            requestBody))
                                                                                    .build();
                                                                    return Mono.fromCompletionStage(
                                                                                    core
                                                                                            .deliverAsync(
                                                                                                    context
                                                                                                            .orElseThrow(),
                                                                                                    authorized,
                                                                                                    new PayerMiddleware
                                                                                                                    .AsyncDelivery<
                                                                                                            ClientResponse>() {
                                                                                                        @Override
                                                                                                        public
                                                                                                        java
                                                                                                                                .util
                                                                                                                                .concurrent
                                                                                                                                .CompletionStage<
                                                                                                                        ClientResponse>
                                                                                                                send(
                                                                                                                        int
                                                                                                                                attemptNo) {
                                                                                                            return next.exchange(
                                                                                                                            paid)
                                                                                                                    .toFuture();
                                                                                                        }

                                                                                                        @Override
                                                                                                        public
                                                                                                        PayerHttpResponse
                                                                                                                view(
                                                                                                                        ClientResponse
                                                                                                                                response) {
                                                                                                            return new PayerHttpResponse(
                                                                                                                    response.statusCode()
                                                                                                                            .value(),
                                                                                                                    PayerMiddleware
                                                                                                                            .copyHeaders(
                                                                                                                                    response.headers()
                                                                                                                                            .asHttpHeaders()),
                                                                                                                    new byte
                                                                                                                            [0]);
                                                                                                        }

                                                                                                        @Override
                                                                                                        public
                                                                                                        java
                                                                                                                                .util
                                                                                                                                .concurrent
                                                                                                                                .CompletionStage<
                                                                                                                        Void>
                                                                                                                discard(
                                                                                                                        ClientResponse
                                                                                                                                response) {
                                                                                                            return response.releaseBody()
                                                                                                                    .toFuture();
                                                                                                        }
                                                                                                    }))
                                                                            .map(
                                                                                    response ->
                                                                                            response.mutate()
                                                                                                    .header(
                                                                                                            "x-agentvault-payment-intent-id",
                                                                                                            authorized
                                                                                                                    .intent()
                                                                                                                    .id())
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
            Call call,
            byte[] requestBody,
            ClientResponse first) {
        if (call == null
                || !SpringPayerMessages.isJson(
                        first.headers().asHttpHeaders().getFirst("content-type"))) {
            return Mono.just(first);
        }
        return bufferWeb(first)
                .flatMap(
                        initial -> {
                            final JsonNode message;
                            try {
                                message = JSON.readTree(initial.body());
                            } catch (IOException error) {
                                return Mono.just(rebuild(initial));
                            }
                            if (!PayerMiddleware.boundMcpResponse(message, call.id(), false)) {
                                return Mono.just(rebuild(initial));
                            }
                            return Mono.fromCallable(
                                            () ->
                                                    core.authorizeMessage(
                                                            new PayerHttpRequest(
                                                                    request.method().name(),
                                                                    target.toString(),
                                                                    PayerMiddleware.origin(target),
                                                                    PayerMiddleware.copyHeaders(
                                                                            request.headers()),
                                                                    requestBody,
                                                                    false),
                                                            message,
                                                            context,
                                                            PayerMiddleware.copyHeaders(
                                                                    initial.response()
                                                                            .headers()
                                                                            .asHttpHeaders())))
                                    .subscribeOn(Schedulers.boundedElastic())
                                    .flatMap(
                                            authorized -> {
                                                if (authorized == null)
                                                    return Mono.just(rebuild(initial));
                                                SpringPayerMessages.event(
                                                        "agentvault.sdk.payer.payment_required",
                                                        context.runId(),
                                                        null,
                                                        null);
                                                SpringPayerMessages.event(
                                                        "agentvault.sdk.payer.signed",
                                                        context.runId(),
                                                        authorized.intent().id(),
                                                        null);
                                                byte[] paidBody;
                                                if (call.a2a() != null)
                                                    paidBody =
                                                            core.a2a()
                                                                    .credential(
                                                                            authorized,
                                                                            call.a2a(),
                                                                            message,
                                                                            PayerMiddleware.origin(
                                                                                    target));
                                                else {
                                                    var metadata =
                                                            core.mcpCredential(
                                                                    authorized,
                                                                    PayerMiddleware.origin(target));
                                                    paidBody =
                                                            messages.credential(
                                                                    call,
                                                                    metadata.key(),
                                                                    metadata.value());
                                                }
                                                ClientRequest paid =
                                                        ClientRequest.from(request)
                                                                .headers(
                                                                        headers -> {
                                                                            headers.remove(
                                                                                    HttpHeaders
                                                                                            .CONTENT_LENGTH);
                                                                            if (call.a2a() != null)
                                                                                headers.set(
                                                                                        "x-a2a-extensions",
                                                                                        core.a2a()
                                                                                                .extensions(
                                                                                                        headers
                                                                                                                .getFirst(
                                                                                                                        "x-a2a-extensions")));
                                                                        })
                                                                .body(
                                                                        BodyInserters.fromValue(
                                                                                paidBody))
                                                                .build();
                                                return Mono.fromCompletionStage(
                                                                core.deliverAsync(
                                                                        context,
                                                                        authorized,
                                                                        new PayerMiddleware
                                                                                        .AsyncDelivery<
                                                                                BufferedWebResponse>() {
                                                                            @Override
                                                                            public java.util
                                                                                                    .concurrent
                                                                                                    .CompletionStage<
                                                                                            BufferedWebResponse>
                                                                                    send(
                                                                                            int
                                                                                                    attemptNo) {
                                                                                return next.exchange(
                                                                                                paid)
                                                                                        .flatMap(
                                                                                                SpringWebFluxPayerMiddleware
                                                                                                                .this
                                                                                                        ::bufferWeb)
                                                                                        .toFuture();
                                                                            }

                                                                            @Override
                                                                            public PayerHttpResponse
                                                                                    view(
                                                                                            BufferedWebResponse
                                                                                                    response) {
                                                                                return new PayerHttpResponse(
                                                                                        response.response()
                                                                                                .statusCode()
                                                                                                .value(),
                                                                                        PayerMiddleware
                                                                                                .copyHeaders(
                                                                                                        response.response()
                                                                                                                .headers()
                                                                                                                .asHttpHeaders()),
                                                                                        new byte
                                                                                                [0]);
                                                                            }

                                                                            @Override
                                                                            public java.util
                                                                                                    .concurrent
                                                                                                    .CompletionStage<
                                                                                            Void>
                                                                                    discard(
                                                                                            BufferedWebResponse
                                                                                                    ignored) {
                                                                                return java.util
                                                                                        .concurrent
                                                                                        .CompletableFuture
                                                                                        .completedFuture(
                                                                                                null);
                                                                            }
                                                                        },
                                                                        response ->
                                                                                Mono.fromRunnable(
                                                                                                () ->
                                                                                                        reportMcp(
                                                                                                                context,
                                                                                                                authorized,
                                                                                                                call,
                                                                                                                response))
                                                                                        .subscribeOn(
                                                                                                Schedulers
                                                                                                        .boundedElastic())
                                                                                        .then()
                                                                                        .toFuture()))
                                                        .map(
                                                                response ->
                                                                        rebuild(
                                                                                response,
                                                                                authorized
                                                                                        .intent()
                                                                                        .id()));
                                            });
                        });
    }

    private ClientRequest captureBody(ClientRequest request, AtomicReference<byte[]> captured) {
        @SuppressWarnings("unchecked")
        BodyInserter<Object, ClientHttpRequest> source =
                (BodyInserter<Object, ClientHttpRequest>) request.body();
        BodyInserter<Void, ClientHttpRequest> body =
                (output, context) ->
                        source.insert(
                                new ClientHttpRequestDecorator(output) {
                                    @Override
                                    public Mono<Void> writeWith(
                                            Publisher<? extends DataBuffer> publisher) {
                                        return DataBufferUtils.join(
                                                        publisher,
                                                        core.config().maxChallengeBytes() + 1)
                                                .flatMap(
                                                        buffer -> {
                                                            byte[] value =
                                                                    new byte
                                                                            [buffer
                                                                                    .readableByteCount()];
                                                            buffer.read(value);
                                                            DataBufferUtils.release(buffer);
                                                            if (value.length
                                                                    > core.config()
                                                                            .maxChallengeBytes()) {
                                                                return Mono.error(
                                                                        new IllegalArgumentException(
                                                                                "paid request body"
                                                                                    + " exceeds"
                                                                                    + " configured"
                                                                                    + " limit"));
                                                            }
                                                            if (A2a.parse(
                                                                            value,
                                                                            core.config()
                                                                                    .maxChallengeBytes())
                                                                    != null)
                                                                getHeaders()
                                                                        .set(
                                                                                "x-a2a-extensions",
                                                                                core.a2a()
                                                                                        .extensions(
                                                                                                getHeaders()
                                                                                                        .getFirst(
                                                                                                                "x-a2a-extensions")));
                                                            captured.set(value);
                                                            return super.writeWith(
                                                                    Mono.just(
                                                                            output.bufferFactory()
                                                                                    .wrap(value)));
                                                        });
                                    }

                                    @Override
                                    public Mono<Void> writeAndFlushWith(
                                            Publisher<? extends Publisher<? extends DataBuffer>>
                                                    body) {
                                        return Mono.error(
                                                new IllegalArgumentException(
                                                        "streaming paid requests cannot be"
                                                                + " replayed"));
                                    }

                                    @Override
                                    public Mono<Void> setComplete() {
                                        captured.compareAndSet(null, new byte[0]);
                                        return super.setComplete();
                                    }
                                },
                                context);
        return ClientRequest.from(request).body(body).build();
    }

    private Mono<BufferedWebResponse> bufferWeb(ClientResponse response) {
        return DataBufferUtils.join(
                        response.body(BodyExtractors.toDataBuffers()),
                        core.config().maxChallengeBytes() + 1)
                .map(
                        buffer -> {
                            byte[] body = new byte[buffer.readableByteCount()];
                            buffer.read(body);
                            DataBufferUtils.release(buffer);
                            if (body.length > core.config().maxChallengeBytes()) {
                                throw new IllegalArgumentException(
                                        "payment response exceeds configured limit");
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
        ClientResponse.Builder builder =
                ClientResponse.create(original.statusCode(), original.strategies())
                        .headers(headers -> headers.putAll(original.headers().asHttpHeaders()))
                        .cookies(cookies -> cookies.putAll(original.cookies()))
                        .request(original.request())
                        .body(
                                Flux.just(
                                        DefaultDataBufferFactory.sharedInstance.wrap(
                                                response.body())));
        if (intentId != null) {
            builder.header("x-agentvault-payment-intent-id", intentId);
        }
        return builder.build();
    }

    private void reportMcp(
            PayerRequestContext context,
            io.agentvault.sdk.model.Payer.AuthorizedPayment authorized,
            Call call,
            BufferedWebResponse response) {
        try {
            messages.report(
                    context,
                    authorized,
                    call,
                    response.response().statusCode().value(),
                    PayerMiddleware.copyHeaders(response.response().headers().asHttpHeaders()),
                    response.body());
        } catch (IOException error) {
            throw new IllegalStateException("paid JSON-RPC response is invalid", error);
        }
    }

    private static Optional<PayerRequestContext> contextFromHeaders(ClientRequest request) {
        String runId = request.headers().getFirst("x-run-id");
        return runId == null ? Optional.empty() : Optional.of(new PayerRequestContext(runId));
    }
}
