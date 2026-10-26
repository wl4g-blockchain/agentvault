package io.agentvault.sdk.model;

import static org.junit.jupiter.api.Assertions.*;

import io.agentvault.sdk.protocol.*;

import org.junit.jupiter.api.Test;

import java.nio.file.*;
import java.util.*;

final class A2aTest {
    @Test
    void sharedContractAndUnsupportedBoundaries() throws Exception {
        var fixture =
                A2a.JSON.readTree(
                        Files.readString(Path.of("..", "tests", "payment_protocol_a2a.json")));
        var registry = new PaymentProtocolFactory().create(List.of("x402", "mpp"));
        var seen = new HashSet<String>();
        for (var row : fixture.get("matrix"))
            seen.add(
                    row.get("transport").asText()
                            + "/"
                            + row.get("protocol").asText()
                            + "/"
                            + row.get("role").asText());
        var expected = new HashSet<String>();
        for (String t : List.of("grpc", "mqtt"))
            for (String p : List.of("x402", "mpp"))
                for (String role : List.of("payer", "payee"))
                    expected.add(t + "/" + p + "/" + role);
        assertEquals(8, fixture.get("matrix").size());
        assertEquals(expected, seen);
        for (String name : List.of("x402", "mpp")) {
            var contract = fixture.get("protocols").get(name);
            assertEquals(
                    name,
                    registry.a2aChallenge(contract.get("challenge_response"), 65536)
                            .adapter()
                            .name());
            var request = contract.get("credential_request").deepCopy();
            var call = A2a.parse(A2a.JSON.writeValueAsBytes(request), 65536);
            assertEquals("task-1", call.params().message().getTaskId());
            assertEquals(name, registry.a2aCredential(request, 65536).protocol());
            assertEquals(
                    name, registry.a2aEvidence(contract.get("settled_response"), 65536).protocol());
            registry.a2aStripCredentials(request);
            assertNull(registry.a2aCredential(request, 65536));
            assertEquals("keep", request.at("/params/message/metadata/application.label").asText());
        }
        for (String method : List.of("message/stream", "SendMessage", "SendStreamingMessage")) {
            var request = A2a.object(fixture.get("request").deepCopy());
            request.put("method", method);
            assertThrows(
                    IllegalArgumentException.class,
                    () -> A2a.parse(A2a.JSON.writeValueAsBytes(request), 65536));
        }
        var request = A2a.object(fixture.at("/protocols/x402/credential_request").deepCopy());
        request.set("_meta", fixture.at("/protocols/mpp/credential_request/_meta"));
        assertThrows(IllegalArgumentException.class, () -> registry.a2aCredential(request, 65536));
        var embedded = fixture.at("/protocols/x402/challenge_response").deepCopy();
        A2a.object(embedded.at("/result/status/message/metadata")).remove("x402.payment.required");
        assertThrows(IllegalArgumentException.class, () -> registry.a2aChallenge(embedded, 65536));
    }
}
