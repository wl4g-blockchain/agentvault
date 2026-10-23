package io.agentvault.sdk.protocol;

import com.fasterxml.jackson.databind.JsonNode;
import java.util.ArrayList;
import java.util.Collections;
import java.util.LinkedHashMap;
import java.util.List;
import java.util.Map;

public final class ProtocolRegistry {
    public record McpChallenge(PaymentProtocol adapter, PaymentProtocol.HttpResponse challenge) {}
    public record McpCredential(String protocol, JsonNode value) {}
    public record McpEvidence(String protocol, PaymentProtocol.MerchantEvidence evidence) {}

    private final Map<String, PaymentProtocol> adapters;

    ProtocolRegistry(Map<String, PaymentProtocol> adapters) {
        this.adapters = Collections.unmodifiableMap(new LinkedHashMap<>(adapters));
    }

    public PaymentProtocol get(String name) {
        PaymentProtocol adapter = adapters.get(name);
        if (adapter == null) throw new IllegalArgumentException("payment protocol is not enabled: " + name);
        return adapter;
    }

    public List<String> credentialHeaderNames() {
        var names = new ArrayList<String>();
        for (PaymentProtocol adapter : adapters.values()) {
            for (String candidate : adapter.credentialHeaderNames()) {
                if (names.stream().noneMatch(existing -> existing.equalsIgnoreCase(candidate))) {
                    names.add(candidate);
                }
            }
        }
        return List.copyOf(names);
    }

    public List<String> merchantEvidenceHeaderNames() {
        var names = new ArrayList<String>();
        for (PaymentProtocol adapter : adapters.values()) {
            for (String candidate : adapter.merchantEvidenceHeaderNames()) {
                if (names.stream().noneMatch(existing -> existing.equalsIgnoreCase(candidate))) {
                    names.add(candidate);
                }
            }
        }
        return List.copyOf(names);
    }

    public McpCredential presentedCredential(Map<String, String> headers, int limit) {
        String selected = null;
        JsonNode payload = null;
        for (PaymentProtocol adapter : adapters.values()) {
            JsonNode value = adapter.presentedCredential(headers, limit);
            if (value == null) continue;
            if (selected != null) {
                throw new IllegalArgumentException("multiple payment protocol credentials were supplied");
            }
            selected = adapter.name();
            payload = value;
        }
        return selected == null ? null : new McpCredential(selected, payload);
    }

    public McpEvidence merchantEvidence(Map<String, String> headers) {
        String selected = null;
        PaymentProtocol.MerchantEvidence evidence = null;
        for (PaymentProtocol adapter : adapters.values()) {
            PaymentProtocol.MerchantEvidence value = adapter.merchantEvidence(headers);
            if (value == null) continue;
            if (selected != null) {
                throw new IllegalArgumentException("multiple payment protocol receipts were supplied");
            }
            selected = adapter.name();
            evidence = value;
        }
        return selected == null ? null : new McpEvidence(selected, evidence);
    }

    public McpChallenge mcpChallenge(JsonNode message, int limit) {
        for (PaymentProtocol adapter : adapters.values()) {
            PaymentProtocol.HttpResponse challenge = adapter.mcpChallenge(message, limit);
            if (challenge != null) return new McpChallenge(adapter, challenge);
        }
        return null;
    }

    public McpCredential presentedMcpCredential(JsonNode meta, int limit) {
        String selected = null;
        JsonNode payload = null;
        for (PaymentProtocol adapter : adapters.values()) {
            JsonNode value = adapter.presentedMcpCredential(meta, limit);
            if (value == null) continue;
            if (selected != null) {
                throw new IllegalArgumentException("multiple MCP payment credentials were supplied");
            }
            selected = adapter.name();
            payload = value;
        }
        return selected == null ? null : new McpCredential(selected, payload);
    }

    public McpEvidence mcpMerchantEvidence(JsonNode meta, int limit) {
        String selected = null;
        PaymentProtocol.MerchantEvidence evidence = null;
        for (PaymentProtocol adapter : adapters.values()) {
            PaymentProtocol.MerchantEvidence value = adapter.mcpMerchantEvidence(meta, limit);
            if (value == null) continue;
            if (selected != null) {
                throw new IllegalArgumentException("multiple MCP settlement receipts were supplied");
            }
            selected = adapter.name();
            evidence = value;
        }
        return selected == null ? null : new McpEvidence(selected, evidence);
    }

    public List<String> mcpRequestMetadataKeys() {
        var keys = new ArrayList<String>();
        for (PaymentProtocol adapter : adapters.values()) {
            keys.add(adapter.mcpRequestMetadataKey());
        }
        return List.copyOf(keys);
    }

    public List<String> mcpRootRequestMetadataKeys() {
        var keys = new ArrayList<String>();
        for (PaymentProtocol adapter : adapters.values()) {
            String key = adapter.mcpRootRequestMetadataKey();
            if (key != null) keys.add(key);
        }
        return List.copyOf(keys);
    }

    public List<String> mcpReceiptMetadataKeys() {
        var keys = new ArrayList<String>();
        for (PaymentProtocol adapter : adapters.values()) {
            keys.add(adapter.mcpReceiptMetadataKey());
        }
        return List.copyOf(keys);
    }
}
