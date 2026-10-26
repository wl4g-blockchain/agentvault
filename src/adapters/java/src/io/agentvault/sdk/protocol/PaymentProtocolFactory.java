package io.agentvault.sdk.protocol;

import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;
import java.util.concurrent.ConcurrentHashMap;
import java.util.function.Supplier;

/** Creates isolated providers and never caches partial or failed instances. */
public final class PaymentProtocolFactory {
    private final Map<String, Supplier<PaymentProtocol>> providers = new ConcurrentHashMap<>();

    public PaymentProtocolFactory() {
        providers.put("x402", X402Adapter::new);
        providers.put("mpp", MppAdapter::new);
    }

    public void register(String name, Supplier<PaymentProtocol> provider) {
        if (name == null
                || !name.equals(name.toLowerCase())
                || provider == null
                || providers.putIfAbsent(name, provider) != null) {
            throw new IllegalArgumentException(
                    "invalid or duplicate payment protocol provider: " + name);
        }
    }

    public ProtocolRegistry create(List<String> enabled) {
        if (enabled == null || enabled.isEmpty()) {
            throw new IllegalArgumentException("payment protocol configuration is empty");
        }
        var adapters = new LinkedHashMap<String, PaymentProtocol>();
        for (String name : enabled) {
            Supplier<PaymentProtocol> provider = providers.get(name);
            if (provider == null) {
                throw new IllegalArgumentException(
                        "payment protocol provider is unavailable: " + name);
            }
            PaymentProtocol adapter = provider.get();
            if (!name.equals(adapter.name()) || adapters.putIfAbsent(name, adapter) != null) {
                throw new IllegalArgumentException(
                        "duplicate or mismatched payment protocol provider: " + name);
            }
        }
        return new ProtocolRegistry(adapters);
    }
}
