package com.wl4g.agentvault.sdk.model;

import static org.junit.jupiter.api.Assertions.assertThrows;

import org.junit.jupiter.api.Test;

final class BaseTest {
    @Test void operationContextRequiresStableIdentifiers() {
        assertThrows(IllegalArgumentException.class, () -> new Base.OperationContext("", "run"));
    }
}
