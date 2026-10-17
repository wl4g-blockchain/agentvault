package io.agentvault.sdk.transport;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.ObjectMapper;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.google.protobuf.BytesValue;
import io.agentvault.sdk.config.Config.GrpcConfig;
import io.agentvault.sdk.model.Base.ApiException;
import io.agentvault.sdk.model.Base.VaultTransport;
import io.grpc.CallOptions;
import io.grpc.Channel;
import io.grpc.ClientInterceptors;
import io.grpc.ManagedChannel;
import io.grpc.ManagedChannelBuilder;
import io.grpc.Metadata;
import io.grpc.MethodDescriptor;
import io.grpc.protobuf.ProtoUtils;
import io.grpc.stub.ClientCalls;
import io.grpc.stub.MetadataUtils;
import java.io.IOException;
import java.util.Base64;
import java.util.concurrent.TimeUnit;

public final class GrpcVaultTransport implements VaultTransport, AutoCloseable {
    private static final ObjectMapper JSON = new ObjectMapper();
    private static final MethodDescriptor<BytesValue, BytesValue> EXECUTE = MethodDescriptor
            .<BytesValue, BytesValue>newBuilder()
            .setType(MethodDescriptor.MethodType.UNARY)
            .setFullMethodName("agentvault.transport.v1.PaymentTransport/Execute")
            .setRequestMarshaller(ProtoUtils.marshaller(BytesValue.getDefaultInstance()))
            .setResponseMarshaller(ProtoUtils.marshaller(BytesValue.getDefaultInstance()))
            .build();

    private final GrpcConfig config;
    private final ManagedChannel managedChannel;
    private final Channel channel;

    public GrpcVaultTransport(GrpcConfig config) {
        this.config = java.util.Objects.requireNonNull(config, "config");
        ManagedChannelBuilder<?> builder = ManagedChannelBuilder.forTarget(config.endpoint())
                .maxInboundMessageSize(config.maxMessageBytes());
        if (config.tls()) builder.useTransportSecurity(); else builder.usePlaintext();
        this.managedChannel = builder.build();
        if (config.authguardBearer() == null) {
            this.channel = managedChannel;
        } else {
            Metadata headers = new Metadata();
            headers.put(Metadata.Key.of("authorization", Metadata.ASCII_STRING_MARSHALLER),
                    "Bearer " + config.authguardBearer());
            this.channel = ClientInterceptors.intercept(
                    managedChannel, MetadataUtils.newAttachHeadersInterceptor(headers));
        }
    }

    @Override public <T> T get(String path, Class<T> type) {
        return execute("GET", path, null, type);
    }

    @Override public <T> T post(String path, Object body, Class<T> type) {
        return execute("POST", path, body, type);
    }

    private <T> T execute(String method, String path, Object body, Class<T> type) {
        if (path == null || !path.startsWith("/api/v1/") || path.indexOf('\\') >= 0) {
            throw new IllegalArgumentException("invalid AgentVault gRPC path");
        }
        try {
            byte[] encodedBody = body == null ? new byte[0] : JSON.writeValueAsBytes(body);
            ObjectNode headers = JSON.createObjectNode();
            headers.put("accept", "application/json");
            if (body != null) headers.put("content-type", "application/json");
            ObjectNode request = JSON.createObjectNode();
            request.put("method", method);
            request.put("path", path);
            request.set("headers", headers);
            request.put("body", Base64.getUrlEncoder().withoutPadding().encodeToString(encodedBody));
            byte[] wire = JSON.writeValueAsBytes(request);
            if (wire.length > config.maxMessageBytes()) {
                throw new IllegalArgumentException("AgentVault gRPC request exceeds configured limit");
            }
            BytesValue response = ClientCalls.blockingUnaryCall(
                    channel,
                    EXECUTE,
                    CallOptions.DEFAULT.withDeadlineAfter(config.timeout().toMillis(), TimeUnit.MILLISECONDS),
                    BytesValue.of(com.google.protobuf.ByteString.copyFrom(wire)));
            byte[] responseBytes = response.getValue().toByteArray();
            if (responseBytes.length > config.maxMessageBytes()) {
                throw new IllegalStateException("AgentVault gRPC response exceeds configured limit");
            }
            JsonNode envelope = JSON.readTree(responseBytes);
            int status = envelope.path("status").asInt(-1);
            String encoded = envelope.path("body").asText(null);
            if (status < 100 || status > 599 || encoded == null) {
                throw new IllegalStateException("invalid AgentVault gRPC response");
            }
            byte[] decoded;
            try {
                decoded = Base64.getUrlDecoder().decode(encoded);
            } catch (IllegalArgumentException error) {
                throw new IllegalStateException("invalid AgentVault gRPC response body", error);
            }
            if (decoded.length > config.maxMessageBytes()
                    || !Base64.getUrlEncoder().withoutPadding().encodeToString(decoded).equals(encoded)) {
                throw new IllegalStateException("invalid or oversized AgentVault gRPC response body");
            }
            JsonNode value = JSON.readTree(decoded);
            if (status < 200 || status >= 300) {
                JsonNode error = value.path("error");
                throw new ApiException(status, error.path("code").asText("HTTP_ERROR"),
                        error.path("message").asText("AgentVault returned HTTP " + status));
            }
            return JSON.treeToValue(value, type);
        } catch (IOException error) {
            throw new IllegalStateException("invalid AgentVault gRPC response", error);
        }
    }

    @Override public void close() {
        managedChannel.shutdown();
        try {
            if (!managedChannel.awaitTermination(5, TimeUnit.SECONDS)) managedChannel.shutdownNow();
        } catch (InterruptedException error) {
            Thread.currentThread().interrupt();
            managedChannel.shutdownNow();
        }
    }

    @Override public String toString() {
        return "GrpcVaultTransport[config=" + config + ", channel=<redacted>]";
    }
}
