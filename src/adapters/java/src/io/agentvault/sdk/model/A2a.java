package io.agentvault.sdk.model;

import com.fasterxml.jackson.databind.*;
import com.fasterxml.jackson.databind.node.ObjectNode;

import io.a2a.spec.*;

import java.util.*;

/** Original JSON-RPC carrier composed with upstream A2A 0.3 models. */
public final class A2a {
    private A2a() {}

    public static final ObjectMapper JSON =
            new ObjectMapper()
                    .findAndRegisterModules()
                    .configure(DeserializationFeature.FAIL_ON_UNKNOWN_PROPERTIES, false);

    public record Call(ObjectNode wire) {
        public JsonNode id() {
            return wire.get("id");
        }

        public MessageSendParams params() {
            MessageSendParams params =
                    JSON.convertValue(wire.get("params"), MessageSendParams.class);
            Message message = params.message();
            if (message == null
                    || message.getRole() != Message.Role.USER
                    || message.getMessageId() == null
                    || message.getMessageId().isBlank()
                    || message.getParts() == null
                    || message.getParts().isEmpty())
                throw new IllegalArgumentException("invalid A2A MessageSendParams");
            return params;
        }

        public void bind(Task task) {
            Message message = params().message();
            if ((message.getTaskId() != null && !message.getTaskId().equals(task.getId()))
                    || (message.getContextId() != null
                            && !message.getContextId().equals(task.getContextId())))
                throw new IllegalArgumentException("A2A task/context does not match request");
            ObjectNode value = object(wire.path("params").path("message"));
            value.put("taskId", task.getId());
            value.put("contextId", task.getContextId());
        }

        public byte[] body() {
            try {
                return JSON.writeValueAsBytes(canonical(wire));
            } catch (Exception error) {
                throw new IllegalArgumentException("A2A message cannot be encoded", error);
            }
        }
    }

    public static Call parse(byte[] bytes, int limit) {
        if (bytes.length > limit)
            throw new IllegalArgumentException("A2A request exceeds configured limit");
        JsonNode value;
        try {
            value = JSON.readTree(bytes);
        } catch (Exception error) {
            return null;
        }
        if (value == null || !value.isObject() || !"2.0".equals(value.path("jsonrpc").asText()))
            return null;
        String method = value.path("method").asText();
        if (List.of("message/stream", "SendMessage", "SendStreamingMessage").contains(method))
            throw new IllegalArgumentException("paid A2A requires non-streaming v0.3 message/send");
        if (!method.equals("message/send")) return null;
        if (!(value.path("id").isTextual() || value.path("id").isNumber()))
            throw new IllegalArgumentException("A2A request requires id");
        Call call = new Call((ObjectNode) value);
        call.params();
        return call;
    }

    public static Task task(JsonNode response) {
        JsonNode value = response.path("result");
        if (!value.isObject() || !"task".equals(value.path("kind").asText()))
            throw new IllegalArgumentException("A2A requires Task response");
        Task task = JSON.convertValue(value, Task.class);
        if (task.getId() == null
                || task.getId().isBlank()
                || task.getContextId() == null
                || task.getContextId().isBlank()
                || task.getStatus() == null
                || task.getStatus().state() == null)
            throw new IllegalArgumentException("invalid A2A Task");
        return task;
    }

    public static ObjectNode response(JsonNode value, JsonNode id) {
        if (!value.isObject()
                || !"2.0".equals(value.path("jsonrpc").asText())
                || !id.equals(value.get("id"))
                || value.has("result") == value.has("error"))
            throw new IllegalArgumentException("A2A response does not match request");
        return (ObjectNode) value;
    }

    public static ObjectNode object(JsonNode node) {
        if (!(node instanceof ObjectNode object))
            throw new IllegalArgumentException("A2A wire value must be an object");
        return object;
    }

    public static ObjectNode metadata(JsonNode value, boolean root) {
        ObjectNode owner = object(root ? value : value.path("params").path("message"));
        String key = root ? "_meta" : "metadata";
        if (!owner.has(key)) owner.set(key, JSON.createObjectNode());
        return object(owner.get(key));
    }

    public static JsonNode canonical(JsonNode value) {
        if (value.isArray()) {
            var result = JSON.createArrayNode();
            value.forEach(item -> result.add(canonical(item)));
            return result;
        }
        if (!value.isObject()) return value;
        var result = JSON.createObjectNode();
        var keys = new TreeSet<String>();
        value.fieldNames().forEachRemaining(keys::add);
        keys.forEach(key -> result.set(key, canonical(value.get(key))));
        return result;
    }
}
