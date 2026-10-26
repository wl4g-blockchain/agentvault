package io.agentvault.sdk.model;

import static org.junit.jupiter.api.Assertions.assertThrows;

import org.junit.jupiter.api.Test;

final class PayeeTest {
    @Test void payeeOperationRejectsAnUnsafeRunId() {
        assertThrows(IllegalArgumentException.class,
                () -> new Payee.PayeeOperation(
                        "scenario", "bad/run", "price", "resource", "origin", "x402", "hash", null));
    }
}
