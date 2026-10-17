package io.agentvault.sdk.model;

import static org.junit.jupiter.api.Assertions.assertThrows;

import org.junit.jupiter.api.Test;

final class PayerTest {
    @Test void payerContextRequiresASafeRunId() {
        assertThrows(IllegalArgumentException.class, () -> new Payer.PayerRequestContext("bad/run"));
    }
}
