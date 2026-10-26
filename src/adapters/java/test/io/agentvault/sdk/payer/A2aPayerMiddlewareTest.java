package io.agentvault.sdk.payer;

import static org.junit.jupiter.api.Assertions.*;

import io.agentvault.sdk.config.Config.PayerConfig;
import io.agentvault.sdk.model.A2a;
import io.agentvault.sdk.model.Payer.*;

import org.junit.jupiter.api.Test;

import java.nio.file.*;
import java.util.*;

final class A2aPayerMiddlewareTest {
    @Test
    void extensionAcknowledgementRequiredBeforeAuthorization() throws Exception {
        var fixture =
                A2a.JSON.readTree(
                        Files.readString(Path.of("..", "tests", "payment_protocol_a2a.json")));
        var transport =
                new JdkHttpPayerMiddlewareTest.PayerTransport(
                        "x402", null, "https://merchant.test/a2a");
        var core = new PayerMiddleware(transport, PayerConfig.defaults("scenario"));
        var request =
                new PayerHttpRequest(
                        "POST",
                        "https://merchant.test/a2a",
                        "https://merchant.test",
                        Map.of(),
                        A2a.JSON.writeValueAsBytes(fixture.get("request")),
                        false);
        assertThrows(
                IllegalArgumentException.class,
                () ->
                        core.a2a()
                                .authorize(
                                        request,
                                        fixture.at("/protocols/x402/challenge_response"),
                                        new PayerRequestContext("run-1"),
                                        Map.of()));
        assertTrue(transport.paths.isEmpty());
        String uri = fixture.get("x402_extension").asText();
        assertEquals(uri, core.a2a().extensions(uri));
    }
}
