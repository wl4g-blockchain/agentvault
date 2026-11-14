package com.wl4g.agentvault.sdk.transport;

import com.wl4g.agentvault.sdk.config.Config.VaultTransportConfig;

public final class VaultTransportFactory {

    public VaultTransport create(VaultTransportConfig config) {
        return new HttpVaultTransport(java.util.Objects.requireNonNull(config).http());
    }
}
