package io.agentvault.sdk.payee;

import io.agentvault.sdk.model.Payee.PayeeOperation;
import io.agentvault.sdk.model.Payee.ProtectionOutcome;

/** Framework-neutral entrypoint for custom Java HTTP/MCP servers. */
public final class StandardPayeeMiddleware {
    private final PayeeMiddleware core;

    public StandardPayeeMiddleware(PayeeMiddleware core) {
        this.core = java.util.Objects.requireNonNull(core, "core");
    }

    public ProtectionOutcome handle(PayeeOperation operation) {
        return core.protect(operation);
    }
}
