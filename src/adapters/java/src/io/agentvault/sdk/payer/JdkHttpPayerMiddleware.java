package io.agentvault.sdk.payer;

import static io.agentvault.sdk.payer.PayerMessages.JSON;

import com.fasterxml.jackson.databind.JsonNode;

import io.agentvault.sdk.model.Payer.AuthorizedPayment;
import io.agentvault.sdk.model.Payer.PayerHttpRequest;
import io.agentvault.sdk.model.Payer.PayerHttpResponse;
import io.agentvault.sdk.model.Payer.PayerRequestContext;
import io.agentvault.sdk.payer.PayerMessages.Call;

import java.io.IOException;
import java.net.URI;
import java.net.http.HttpClient;
import java.net.http.HttpRequest;
import java.net.http.HttpResponse;
import java.util.LinkedHashMap;
import java.util.Map;

/** Spring-independent JDK HttpClient payer wrapper with replayable byte bodies. */
public final class JdkHttpPayerMiddleware {

    public record Result(HttpResponse<byte[]> response, AuthorizedPayment payment) {}

    private final PayerMiddleware core;
    private final HttpClient client;
    private final PayerMessages messages;

    public JdkHttpPayerMiddleware(PayerMiddleware core) {
        this(core, HttpClient.newBuilder().followRedirects(HttpClient.Redirect.NEVER).build());
    }

    public JdkHttpPayerMiddleware(PayerMiddleware core, HttpClient client) {
        this.core = java.util.Objects.requireNonNull(core, "core");
        this.client = java.util.Objects.requireNonNull(client, "client");
        this.messages = new PayerMessages(core);
        if (client.followRedirects() != HttpClient.Redirect.NEVER) {
            throw new IllegalArgumentException("payer HTTP client must disable redirects");
        }
    }

    public Result send(HttpRequest request, byte[] replayableBody, PayerRequestContext context)
            throws IOException, InterruptedException {
        java.util.Objects.requireNonNull(request, "request");
        byte[] body = replayableBody == null ? new byte[0] : replayableBody.clone();
        if (body.length > core.config().maxChallengeBytes()) {
            throw new IllegalArgumentException("paid request body exceeds configured limit");
        }
        Call call = messages.parse(request.method(), headers(request.headers().map()), body);
        if (call != null && call.a2a() != null && context != null)
            request =
                    copy(
                            request,
                            body,
                            Map.entry(
                                    "x-a2a-extensions",
                                    core.a2a()
                                            .extensions(
                                                    request.headers()
                                                            .firstValue("x-a2a-extensions")
                                                            .orElse(""))));
        if (call != null && call.a2a() != null && context != null) {
            String run = request.headers().firstValue("x-run-id").orElse(context.runId());
            if (!run.equals(context.runId()))
                throw new IllegalArgumentException("A2A run_id conflict");
            request = copy(request, body, Map.entry("x-run-id", run));
        }
        HttpRequest firstRequest = copy(request, body, null);
        HttpResponse<byte[]> first =
                client.send(firstRequest, HttpResponse.BodyHandlers.ofByteArray());
        if (context == null) return new Result(first, null);
        URI target = PayerMiddleware.safeUri(request.uri().toString());
        if (first.statusCode() != 402) {
            Result paid = handleMcpChallenge(request, body, call, first, target, context);
            return paid == null ? new Result(first, null) : paid;
        }
        AuthorizedPayment authorized =
                core.authorize(
                        new PayerHttpRequest(
                                request.method(),
                                target.toString(),
                                PayerMiddleware.origin(target),
                                headers(request.headers().map()),
                                body,
                                false),
                        new PayerHttpResponse(
                                first.statusCode(), headers(first.headers().map()), first.body()),
                        context);
        Map.Entry<String, String> delivery =
                core.credentialHeader(
                        authorized,
                        PayerMiddleware.origin(target),
                        headers(request.headers().map()));
        HttpRequest paid = copy(request, body, delivery);
        final HttpResponse<byte[]> response;
        try {
            response =
                    core.deliver(
                            context,
                            authorized,
                            ignored -> {
                                try {
                                    return client.send(
                                            paid, HttpResponse.BodyHandlers.ofByteArray());
                                } catch (InterruptedException error) {
                                    Thread.currentThread().interrupt();
                                    throw new InterruptedDelivery(error);
                                }
                            },
                            new PayerMiddleware.DeliveryResponse<
                                    HttpResponse<byte[]>, IOException>() {
                                @Override
                                public PayerHttpResponse view(HttpResponse<byte[]> value) {
                                    return new PayerHttpResponse(
                                            value.statusCode(),
                                            headers(value.headers().map()),
                                            new byte[0]);
                                }

                                @Override
                                public void discard(HttpResponse<byte[]> value) {
                                    // Byte-array responses are already fully consumed.
                                }
                            });
        } catch (InterruptedDelivery error) {
            throw error.interrupted;
        }
        return new Result(response, authorized);
    }

    private Result handleMcpChallenge(
            HttpRequest request,
            byte[] body,
            Call call,
            HttpResponse<byte[]> first,
            URI target,
            PayerRequestContext context)
            throws IOException, InterruptedException {
        if (call == null || !isJson(first.headers().firstValue("content-type").orElse(null)))
            return null;
        if (first.body().length > core.config().maxChallengeBytes()) {
            throw new IOException("payment challenge exceeds configured limit");
        }
        final JsonNode message;
        try {
            message = JSON.readTree(first.body());
        } catch (IOException error) {
            return null;
        }
        if (!PayerMiddleware.boundMcpResponse(message, call.id(), false)) return null;
        AuthorizedPayment authorized =
                core.authorizeMessage(
                        new PayerHttpRequest(
                                request.method(),
                                target.toString(),
                                PayerMiddleware.origin(target),
                                headers(request.headers().map()),
                                body,
                                false),
                        message,
                        context,
                        headers(first.headers().map()));
        if (authorized == null) return null;
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
        HttpRequest paid = copy(request, paidBody, null);
        final HttpResponse<byte[]> response;
        try {
            response =
                    core.deliver(
                            context,
                            authorized,
                            ignored -> {
                                try {
                                    return client.send(
                                            paid, HttpResponse.BodyHandlers.ofByteArray());
                                } catch (InterruptedException error) {
                                    Thread.currentThread().interrupt();
                                    throw new InterruptedDelivery(error);
                                }
                            },
                            new PayerMiddleware.DeliveryResponse<
                                    HttpResponse<byte[]>, IOException>() {
                                @Override
                                public PayerHttpResponse view(HttpResponse<byte[]> value) {
                                    return new PayerHttpResponse(
                                            value.statusCode(),
                                            headers(value.headers().map()),
                                            new byte[0]);
                                }

                                @Override
                                public void discard(HttpResponse<byte[]> ignored) {
                                    // Byte-array responses are already fully consumed.
                                }
                            },
                            value ->
                                    messages.report(
                                            context,
                                            authorized,
                                            call,
                                            value.statusCode(),
                                            headers(value.headers().map()),
                                            value.body()));
        } catch (InterruptedDelivery error) {
            throw error.interrupted;
        }
        return new Result(response, authorized);
    }

    private static boolean isJson(String value) {
        return value != null
                && value.toLowerCase(java.util.Locale.ROOT).startsWith("application/json");
    }

    private HttpRequest copy(
            HttpRequest source, byte[] body, Map.Entry<String, String> credential) {
        HttpRequest.Builder builder =
                HttpRequest.newBuilder(source.uri())
                        .method(
                                source.method(),
                                body.length == 0
                                        ? HttpRequest.BodyPublishers.noBody()
                                        : HttpRequest.BodyPublishers.ofByteArray(body))
                        .expectContinue(source.expectContinue());
        source.timeout().ifPresent(builder::timeout);
        source.version().ifPresent(builder::version);
        source.headers()
                .map()
                .forEach(
                        (name, values) -> {
                            if (!name.equalsIgnoreCase("content-length")) {
                                values.forEach(value -> builder.header(name, value));
                            }
                        });
        if (credential != null) builder.setHeader(credential.getKey(), credential.getValue());
        return builder.build();
    }

    private static Map<String, String> headers(Map<String, java.util.List<String>> values) {
        var headers = new LinkedHashMap<String, String>();
        values.forEach((name, entries) -> headers.put(name, String.join(", ", entries)));
        return headers;
    }

    private static final class InterruptedDelivery extends RuntimeException {
        private final InterruptedException interrupted;

        private InterruptedDelivery(InterruptedException interrupted) {
            super(interrupted);
            this.interrupted = interrupted;
        }
    }
}
