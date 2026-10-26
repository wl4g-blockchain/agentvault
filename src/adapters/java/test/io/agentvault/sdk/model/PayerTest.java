package io.agentvault.sdk.model;

import static org.junit.jupiter.api.Assertions.assertThrows;
import static org.junit.jupiter.api.Assertions.assertFalse;
import static org.junit.jupiter.api.Assertions.assertEquals;

import com.fasterxml.jackson.databind.ObjectMapper;

import org.junit.jupiter.api.Test;

final class PayerTest {
    @Test void payerContextRequiresASafeRunId() {
        assertThrows(IllegalArgumentException.class, () -> new Payer.PayerRequestContext("bad/run"));
    }

    @Test void receiptReportRequiresProtocolEvidence() throws Exception {
        var mapper = new ObjectMapper();
        var request = new Payer.ReportOutboundResponseRequest(
                "scenario", "run", "mpp", new Payer.ProtocolEvidence("payment-receipt", "receipt"));
        var wire = mapper.readTree(mapper.writeValueAsBytes(request));
        assertEquals("mpp", wire.path("protocol").asText());
        assertEquals("payment-receipt", wire.path("evidence").path("header_name").asText());
        assertFalse(wire.has("payment_response"));
        assertThrows(IllegalArgumentException.class,
                () -> new Payer.ReportOutboundResponseRequest("scenario", "run", "mpp", null));
        assertThrows(com.fasterxml.jackson.databind.JsonMappingException.class,
                () -> mapper.readValue("""
                        {"scenario_id":"scenario","run_id":"run","payment_response":"receipt"}
                        """, Payer.ReportOutboundResponseRequest.class));
    }
}
