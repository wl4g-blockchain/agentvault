package io.agentvault.sdk.model;

import static org.junit.jupiter.api.Assertions.assertThrows;

import org.junit.jupiter.api.Test;

final class PayeeTest {
    @Test void payeeOperationRejectsAnUnstableExecutionKey() {
        assertThrows(IllegalArgumentException.class,
                () -> new Payee.PayeeOperation(
                        "scenario", "run", "bad key", "offer", "resource", "origin", "x402", "hash", null));
    }
}
