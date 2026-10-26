package io.agentvault.sdk.payee;

import com.fasterxml.jackson.databind.JsonNode;
import com.fasterxml.jackson.databind.node.ObjectNode;

import io.a2a.server.tasks.TaskStore;
import io.a2a.spec.*;
import io.agentvault.sdk.model.A2a;
import io.agentvault.sdk.model.Base.PaymentChallenge;
import io.agentvault.sdk.model.Payee.SettledIncomingPayment;
import io.agentvault.sdk.protocol.*;

import java.util.*;

/** Composes the application's upstream TaskStore; never owns a second task database. */
public final class A2aPayeeMiddleware {
    public record Call(A2a.Call message, Task task, TaskStore store, JsonNode credential) {
        public JsonNode id() {
            return message.id();
        }

        public byte[] canonicalBody() {
            return message.body();
        }
    }

    private static final List<String> SCOPE =
            List.of(
                    "agentvault.payment.run_id",
                    "agentvault.payment.scenario_id",
                    "agentvault.payment.offer_id");
    private final PayeeMiddleware core;
    private final ProtocolRegistry protocols;

    A2aPayeeMiddleware(PayeeMiddleware core, ProtocolRegistry protocols) {
        this.core = core;
        this.protocols = protocols;
    }

    public String extension(String protocol) {
        return protocols.get(protocol).a2aExtension();
    }

    public AgentCard agentCard(AgentCard card) {
        var value = A2a.object(A2a.JSON.valueToTree(card));
        var caps = A2a.object(value.get("capabilities"));
        if (!caps.hasNonNull("extensions")) caps.putArray("extensions");
        var extensions = (com.fasterxml.jackson.databind.node.ArrayNode) caps.get("extensions");
        for (String uri : protocols.a2aExtensions("").split(", "))
            if (!uri.isEmpty()) {
                boolean exists = false;
                for (var item : extensions)
                    if (uri.equals(item.path("uri").asText())) exists = true;
                if (!exists) extensions.addObject().put("uri", uri).put("required", true);
            }
        return A2a.JSON.convertValue(value, AgentCard.class);
    }

    public Call prepare(
            byte[] body,
            String runId,
            String scenario,
            String offer,
            String protocol,
            TaskStore store,
            Map<String, String> headers) {
        var call = A2a.parse(body, core.config().maxBodyBytes());
        if (call == null) return null;
        if (runId == null || runId.isBlank() || store == null)
            throw new IllegalArgumentException("A2A requires run_id and application TaskStore");
        var adapter = protocols.get(protocol);
        String extension = adapter.a2aExtension();
        if (!extension.isEmpty()
                && !Arrays.asList(
                                Optional.ofNullable(adapter.header(headers, "x-a2a-extensions"))
                                        .orElse("")
                                        .split("\\s*,\\s*"))
                        .contains(extension))
            throw new IllegalArgumentException("A2A extension not activated");
        var credential = protocols.a2aCredential(call.wire(), core.config().maxBodyBytes());
        if (credential != null && !credential.protocol().equals(protocol))
            throw new IllegalArgumentException("A2A credential protocol mismatch");
        protocols.a2aStripCredentials(call.wire());
        Message message = call.params().message();
        String taskId = message.getTaskId() == null ? runId : message.getTaskId();
        Task task = store.get(taskId);
        if (task == null) {
            if (credential != null || message.getTaskId() != null)
                throw new IllegalArgumentException(
                        "A2A task must exist before credential or continuation");
            task =
                    new Task(
                            taskId,
                            message.getContextId() == null
                                    ? UUID.randomUUID().toString()
                                    : message.getContextId(),
                            new TaskStatus(TaskState.SUBMITTED),
                            null,
                            List.of(message),
                            null);
        }
        var value = A2a.object(A2a.JSON.valueToTree(task));
        if (!value.hasNonNull("metadata")) value.putObject("metadata");
        var meta = A2a.object(value.get("metadata"));
        var values = List.of(runId, scenario, offer);
        for (int i = 0; i < SCOPE.size(); i++) {
            String key = SCOPE.get(i);
            if ((meta.has(key) && !values.get(i).equals(meta.get(key).asText()))
                    || (!meta.has(key) && credential != null))
                throw new IllegalArgumentException("A2A task belongs to another payment scope");
            meta.put(key, values.get(i));
        }
        task = A2a.JSON.convertValue(value, Task.class);
        call.bind(task);
        store.save(task);
        return new Call(call, task, store, credential == null ? null : credential.value());
    }

    public PaymentProtocol.McpPaymentRequired challenge(
            Call call, PaymentChallenge challenge, boolean rejected) {
        var wire =
                protocols
                        .get(challenge.protocol())
                        .a2aPaymentRequired(challenge, call.task(), rejected);
        if (wire.result() != null)
            persist(call, A2a.task(A2a.JSON.createObjectNode().set("result", wire.result())));
        return wire;
    }

    public ObjectNode receipt(
            Call call, JsonNode raw, PaymentChallenge challenge, SettledIncomingPayment payment) {
        var message = A2a.response(raw, call.id());
        if (protocols.a2aEvidence(message, core.config().maxBodyBytes()) != null)
            throw new IllegalArgumentException("handler supplied payment receipt");
        if (message.has("error")) {
            var task = A2a.object(A2a.JSON.valueToTree(call.task()));
            var status = task.putObject("status");
            status.put("state", "failed");
            var detail = status.putObject("message");
            detail.put("kind", "message");
            detail.put("role", "agent");
            detail.put("messageId", UUID.randomUUID().toString());
            detail.put("taskId", call.task().getId());
            detail.put("contextId", call.task().getContextId());
            detail.putArray("parts")
                    .addObject()
                    .put("kind", "data")
                    .putObject("data")
                    .set("jsonrpc_error", message.remove("error"));
            message.set("result", task);
        }
        call.message().bind(A2a.task(message));
        protocols.get(payment.receipt().protocol()).a2aInjectReceipt(message, challenge, payment);
        persist(call, A2a.task(message));
        return message;
    }

    private void persist(Call call, Task task) {
        if (!call.task().getId().equals(task.getId())
                || !call.task().getContextId().equals(task.getContextId()))
            throw new IllegalArgumentException("A2A response changed task/context");
        var meta = new LinkedHashMap<String, Object>();
        if (task.getMetadata() != null) meta.putAll(task.getMetadata());
        for (String key : SCOPE) {
            Object value = call.task().getMetadata().get(key);
            if (meta.containsKey(key) && !Objects.equals(value, meta.get(key)))
                throw new IllegalArgumentException("handler changed payment scope");
            meta.put(key, value);
        }
        call.store()
                .save(
                        new Task(
                                task.getId(),
                                task.getContextId(),
                                task.getStatus(),
                                task.getArtifacts(),
                                task.getHistory(),
                                meta));
    }
}
