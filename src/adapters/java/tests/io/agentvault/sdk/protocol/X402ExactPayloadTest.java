package io.agentvault.sdk.protocol;

import static org.junit.jupiter.api.Assertions.assertInstanceOf;
import static org.junit.jupiter.api.Assertions.assertNotNull;
import static org.junit.jupiter.api.Assertions.assertThrows;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import org.junit.jupiter.api.Test;

final class X402ExactPayloadTest {
    private static final ObjectMapper JSON = new ObjectMapper();

    @Test void parsesTheChainSpecificOfficialWireShapes() throws Exception {
        JsonNode fixture = fixture();
        JsonNode evm = fixture.at("/cases/evm_eip3009/payment_payload");
        JsonNode solana = fixture.at("/cases/solana_spl_exact/payment_payload");

        assertInstanceOf(
                X402ExactPayload.Eip3009.class,
                X402ExactPayload.parse(JSON, evm.path("accepted"), evm.path("payload"), "EVM"));
        assertInstanceOf(
                X402ExactPayload.SolanaExact.class,
                X402ExactPayload.parse(
                        JSON, solana.path("accepted"), solana.path("payload"), "Solana"));
    }

    @Test void rejectsUnknownAndWeaklyTypedFields() throws Exception {
        JsonNode evm = fixture().at("/cases/evm_eip3009/payment_payload");
        var unknown = evm.deepCopy();
        ((com.fasterxml.jackson.databind.node.ObjectNode) unknown.path("payload"))
                .put("unexpected", true);
        assertThrows(IllegalArgumentException.class, () -> X402ExactPayload.parse(
                JSON, unknown.path("accepted"), unknown.path("payload"), "EVM"));

        var wrongType = evm.deepCopy();
        ((com.fasterxml.jackson.databind.node.ObjectNode)
                wrongType.at("/payload/authorization")).put("validAfter", 0);
        assertThrows(IllegalArgumentException.class, () -> X402ExactPayload.parse(
                JSON, wrongType.path("accepted"), wrongType.path("payload"), "EVM"));
    }

    private JsonNode fixture() throws Exception {
        var stream = getClass().getClassLoader().getResourceAsStream("x402_v2_chain_payloads.json");
        assertNotNull(stream, "shared x402 fixture");
        try (stream) {
            return JSON.readTree(stream);
        }
    }
}
