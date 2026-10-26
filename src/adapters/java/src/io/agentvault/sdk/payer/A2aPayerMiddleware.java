package io.agentvault.sdk.payer;

import com.fasterxml.jackson.databind.JsonNode;

import io.agentvault.sdk.model.A2a;
import io.agentvault.sdk.model.Payer.*;
import io.agentvault.sdk.protocol.ProtocolRegistry;

import java.util.*;

/** A2A projection onto the existing payer authorization and delivery state machine. */
public final class A2aPayerMiddleware {
    private final PayerMiddleware core;
    private final ProtocolRegistry protocols;

    A2aPayerMiddleware(PayerMiddleware core, ProtocolRegistry protocols) {
        this.core = core;
        this.protocols = protocols;
    }

    public String extensions(String existing) {
        return protocols.a2aExtensions(existing);
    }

    public AuthorizedPayment authorize(
            PayerHttpRequest request,
            JsonNode raw,
            PayerRequestContext context,
            Map<String, String> headers) {
        var call = A2a.parse(request.body(), core.config().maxChallengeBytes());
        if (call == null) return null;
        var message = A2a.response(raw, call.id());
        var selected = protocols.a2aChallenge(message, core.config().maxChallengeBytes());
        if (selected == null) return null;
        if (PayerMiddleware.header(headers, "payment-required") != null
                || PayerMiddleware.header(headers, "www-authenticate") != null)
            throw new IllegalArgumentException("A2A challenge conflicts with HTTP authentication");
        String extension = selected.adapter().a2aExtension();
        if (!extension.isEmpty()
                && !Arrays.asList(
                                Optional.ofNullable(
                                                PayerMiddleware.header(headers, "x-a2a-extensions"))
                                        .orElse("")
                                        .split("\\s*,\\s*"))
                        .contains(extension))
            throw new IllegalArgumentException("A2A extension was not acknowledged");
        var projected = new LinkedHashMap<>(selected.challenge().headers());
        projected.put("payment-protocol", selected.adapter().name());
        String expiry = PayerMiddleware.header(headers, "payment-expires-at");
        if (expiry != null) projected.put("payment-expires-at", expiry);
        return core.authorize(
                request,
                new PayerHttpResponse(402, projected, selected.challenge().body()),
                context);
    }

    public byte[] credential(
            AuthorizedPayment payment, A2a.Call call, JsonNode challenge, String origin) {
        var adapter = protocols.get(payment.credential().protocol());
        if (!payment.intent().protocol().equals(adapter.name())
                || !origin.equalsIgnoreCase(adapter.credential(payment.credential()).boundOrigin()))
            throw new IllegalArgumentException("A2A credential protocol/origin mismatch");
        if (protocols.a2aCredential(call.wire(), core.config().maxChallengeBytes()) != null)
            throw new IllegalArgumentException("A2A request already contains credential");
        var response = A2a.response(challenge, call.id());
        if (response.has("result")) call.bind(A2a.task(response));
        adapter.a2aApplyCredential(call.wire(), payment.credential());
        return call.body();
    }

    public void report(
            PayerRequestContext context,
            AuthorizedPayment payment,
            A2a.Call call,
            JsonNode raw,
            Map<String, String> headers) {
        var message = A2a.response(raw, call.id());
        if (message.has("result")) call.bind(A2a.task(message));
        var selected = protocols.a2aEvidence(message, core.config().maxChallengeBytes());
        if (selected == null || !selected.protocol().equals(payment.credential().protocol()))
            throw new IllegalArgumentException(
                    "matching A2A receipt missing; payment remains pending");
        String previous = PayerMiddleware.header(headers, selected.evidence().headerName());
        if (previous != null && !previous.equals(selected.evidence().headerValue()))
            throw new IllegalArgumentException("A2A/HTTP receipts conflict");
        core.report(
                context,
                payment,
                new PayerHttpResponse(
                        200,
                        Map.of(selected.evidence().headerName(), selected.evidence().headerValue()),
                        new byte[0]));
    }
}
