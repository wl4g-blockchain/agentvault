package io.agentvault.sdk.payee;

import static org.junit.jupiter.api.Assertions.*;

import com.fasterxml.jackson.databind.*;
import com.fasterxml.jackson.databind.node.ObjectNode;
import com.sun.net.httpserver.HttpServer;

import io.a2a.server.tasks.TaskStore;
import io.a2a.spec.*;
import io.agentvault.sdk.config.Config.*;
import io.agentvault.sdk.model.A2a;
import io.agentvault.sdk.model.Payer.PayerRequestContext;
import io.agentvault.sdk.payer.*;
import io.agentvault.sdk.protocol.MppAdapter;
import io.agentvault.sdk.transport.VaultTransport;

import org.junit.jupiter.api.Test;
import org.springframework.mock.web.*;

import java.net.*;
import java.net.http.*;
import java.nio.file.*;
import java.util.*;
import java.util.concurrent.atomic.*;

final class A2aPayeeMiddlewareTest {
    private static final ObjectMapper JSON = new ObjectMapper().findAndRegisterModules();

    static final class Store implements TaskStore {
        private final Map<String, Task> tasks = new java.util.concurrent.ConcurrentHashMap<>();

        public Task get(String id) {
            return tasks.get(id);
        }

        public void save(Task task) {
            tasks.put(task.getId(), task);
        }

        public void delete(String id) {
            tasks.remove(id);
        }
    }

    @Test
    void bothRolesBothProtocolsPreserveTaskAndReceiptOnBusinessFailure() throws Exception {
        var carrier =
                JSON.readTree(
                        Files.readString(Path.of("..", "tests", "payment_protocol_a2a.json")));
        var contracts =
                JSON.readTree(Files.readString(Path.of("..", "tests", "payment_protocol_mcp.json")))
                        .get("protocols");
        for (String protocol : List.of("x402", "mpp"))
            for (boolean failed : List.of(false, true))
                for (String client : List.of("jdk", "spring", "webflux")) {
                    var store = new Store();
                    store.save(
                            new Task(
                                    "task-1",
                                    "context-1",
                                    new TaskStatus(TaskState.SUBMITTED),
                                    null,
                                    null,
                                    null));
                    var inbound = new PayeeTransport(protocol, contracts.get(protocol), true);
                    var payee = new PayeeMiddleware(inbound, PayeeConfig.defaults());
                    var filter =
                            payee.spring(
                                    List.of(
                                            SpringInterceptorPayeeMiddleware.Route.a2a(
                                                    "/a2a",
                                                    "scenario",
                                                    "price-1",
                                                    "mcp://tool/marketpulse.realtime",
                                                    "https://merchant.test",
                                                    protocol,
                                                    store)));
                    var executions = new AtomicInteger();
                    var failure = new AtomicReference<Throwable>();
                    var server = HttpServer.create(new InetSocketAddress("127.0.0.1", 0), 0);
                    server.createContext(
                            "/a2a",
                            exchange -> {
                                try {
                                    var request = new MockHttpServletRequest("POST", "/a2a");
                                    request.setContent(exchange.getRequestBody().readAllBytes());
                                    exchange.getRequestHeaders()
                                            .forEach(
                                                    (name, values) ->
                                                            values.forEach(
                                                                    value ->
                                                                            request.addHeader(
                                                                                    name, value)));
                                    var response = new MockHttpServletResponse();
                                    filter.doFilter(
                                            request,
                                            response,
                                            (req, res) -> {
                                                executions.incrementAndGet();
                                                assertEquals(
                                                        "Bearer application-token",
                                                        request.getHeader("authorization"));
                                                var call =
                                                        A2a.parse(
                                                                req.getInputStream().readAllBytes(),
                                                                65536);
                                                assertEquals(
                                                        "task-1",
                                                        call.params().message().getTaskId());
                                                assertEquals(
                                                        Map.of("application.label", "keep"),
                                                        call.params().message().getMetadata());
                                                var value = JSON.createObjectNode();
                                                value.put("jsonrpc", "2.0");
                                                value.set("id", call.id());
                                                if (failed)
                                                    value.putObject("error")
                                                            .put("code", -32603)
                                                            .put("message", "business failed");
                                                else
                                                    value.set(
                                                            "result",
                                                            JSON.valueToTree(
                                                                    new Task(
                                                                            "task-1",
                                                                            "context-1",
                                                                            new TaskStatus(
                                                                                    TaskState
                                                                                            .COMPLETED),
                                                                            null,
                                                                            null,
                                                                            null)));
                                                res.setContentType("application/json");
                                                JSON.writeValue(res.getOutputStream(), value);
                                            });
                                    response.getHeaderNames()
                                            .forEach(
                                                    name ->
                                                            exchange.getResponseHeaders()
                                                                    .put(
                                                                            name,
                                                                            new ArrayList<>(
                                                                                    response
                                                                                            .getHeaders(
                                                                                                    name))));
                                    byte[] body = response.getContentAsByteArray();
                                    exchange.sendResponseHeaders(response.getStatus(), body.length);
                                    exchange.getResponseBody().write(body);
                                } catch (Throwable error) {
                                    failure.set(error);
                                    exchange.sendResponseHeaders(500, -1);
                                } finally {
                                    exchange.close();
                                }
                            });
                    server.start();
                    try {
                        String url = "http://127.0.0.1:" + server.getAddress().getPort() + "/a2a";
                        var outbound = new PayerTransport(protocol, contracts.get(protocol), url);
                        var payer = new PayerMiddleware(outbound, PayerConfig.defaults("scenario"));
                        byte[] body = JSON.writeValueAsBytes(carrier.get("request"));
                        byte[] result;
                        if (client.equals("jdk")) {
                            var request =
                                    HttpRequest.newBuilder(URI.create(url))
                                            .header("content-type", "application/json")
                                            .header("authorization", "Bearer application-token")
                                            .POST(HttpRequest.BodyPublishers.ofByteArray(body))
                                            .build();
                            var response =
                                    new JdkHttpPayerMiddleware(payer)
                                            .send(request, body, new PayerRequestContext("run-1"));
                            assertEquals(
                                    200,
                                    response.response().statusCode(),
                                    new String(response.response().body()));
                            result = response.response().body();
                        } else if (client.equals("webflux")) {
                            var webClient =
                                    org.springframework.web.reactive.function.client.WebClient
                                            .builder()
                                            .clientConnector(
                                                    new org.springframework.http.client.reactive
                                                            .JdkClientHttpConnector(
                                                            HttpClient.newBuilder()
                                                                    .followRedirects(
                                                                            HttpClient.Redirect
                                                                                    .NEVER)
                                                                    .build()))
                                            .filter(payer.spring().webClientFilter())
                                            .build();
                            result =
                                    webClient
                                            .post()
                                            .uri(url)
                                            .header("x-run-id", "run-1")
                                            .header("authorization", "Bearer application-token")
                                            .header("content-type", "application/json")
                                            .bodyValue(body)
                                            .retrieve()
                                            .bodyToMono(byte[].class)
                                            .block(java.time.Duration.ofSeconds(10));
                        } else {
                            result =
                                    new SpringRestPayerMiddleware(payer)
                                            .client()
                                            .post()
                                            .uri(url)
                                            .header("x-run-id", "run-1")
                                            .header("authorization", "Bearer application-token")
                                            .header("content-type", "application/json")
                                            .body(body)
                                            .retrieve()
                                            .body(byte[].class);
                        }
                        assertNull(failure.get());
                        assertEquals(
                                failed ? "failed" : "completed",
                                JSON.readTree(result).at("/result/status/state").asText());
                        assertEquals(1, executions.get());
                        assertEquals(1, new HashSet<>(inbound.requestHashes).size());
                        assertEquals(2, outbound.paths.size());
                        assertEquals(protocol, outbound.report.get("protocol").asText());
                        assertEquals(
                                "run-1",
                                store.get("task-1").getMetadata().get("agentvault.payment.run_id"));
                    } finally {
                        server.stop(0);
                    }
                }
    }

    private static final class PayeeTransport implements VaultTransport {
        private final String protocol;
        private final JsonNode contract;
        private final boolean valid;
        private final List<String> paths = new ArrayList<>();
        private final List<String> requestHashes = new ArrayList<>();
        private String presentedCredentialProtocol;

        private PayeeTransport(String protocol, JsonNode contract, boolean valid) {
            this.protocol = protocol;
            this.contract = contract;
            this.valid = valid;
        }

        @Override
        public <T> T get(String path, Class<T> type) {
            throw new UnsupportedOperationException();
        }

        @Override
        public <T> T post(String path, Object body, Class<T> type) {
            paths.add(path);
            JsonNode request = JSON.valueToTree(body);
            try {
                if (path.equals("/api/v1/payee/challenges")) {
                    requestHashes.add(request.path("request_hash").asText());
                    ObjectNode challenge = JSON.createObjectNode();
                    challenge.put("scenario_id", "scenario");
                    challenge.put("price_id", "price-1");
                    challenge.put("resource", "mcp://tool/marketpulse.realtime");
                    challenge.put("protocol", protocol);
                    challenge
                            .putObject("asset")
                            .put("network", "eip155:42431")
                            .put("asset", "USDC")
                            .put("decimals", 6);
                    challenge.put("amount", "1000");
                    challenge.put("recipient", "0x1111111111111111111111111111111111111111");
                    challenge.put(
                            "nonce",
                            protocol.equals("mpp") ? "challenge-0123456789abcdef" : "nonce-1");
                    challenge.put("challenge_hash", "challenge-1");
                    challenge.put("expires_at", 2_000_000_000L);
                    ObjectNode payload = challenge.putObject("payload");
                    if (protocol.equals("x402")) {
                        payload.set(
                                "payment_required",
                                contract.path("challenge_response")
                                        .path("result")
                                        .path("structuredContent"));
                    } else {
                        payload.set(
                                "mcp_challenge",
                                contract.path("challenge_response")
                                        .path("error")
                                        .path("data")
                                        .path("challenges")
                                        .get(0));
                    }
                    return JSON.treeToValue(challenge, type);
                }
                if (path.equals("/api/v1/payee/verify")) {
                    presentedCredentialProtocol =
                            request.path("credential").path("protocol").asText();
                    ObjectNode response = JSON.createObjectNode();
                    response.set("intent", intent(false));
                    response.put("valid", valid);
                    if (valid) response.put("verification_ref", "verification-1");
                    return JSON.treeToValue(response, type);
                }
                if (path.equals("/api/v1/payee/settle")) {
                    ObjectNode response = JSON.createObjectNode();
                    response.set("intent", intent(true));
                    ObjectNode receipt = response.putObject("receipt");
                    receipt.put("intent_id", "intent-1");
                    receipt.put("scenario_id", "scenario");
                    receipt.put("direction", "inbound");
                    receipt.put("protocol", protocol);
                    receipt.putObject("asset")
                            .put("network", "eip155:42431")
                            .put("asset", "USDC")
                            .put("decimals", 6);
                    receipt.put("recipient", "0x1111111111111111111111111111111111111111");
                    receipt.put("payer", "0x3333333333333333333333333333333333333333");
                    receipt.put("requested_amount", "1000");
                    receipt.put("settled_amount", "1000");
                    receipt.put("status", "settled");
                    receipt.put("tx_hash", "0x1111");
                    receipt.put("evidence_hash", "evidence-1");
                    receipt.put("confirmed_at", 2_000_000_000L);
                    return JSON.treeToValue(response, type);
                }
                throw new AssertionError("unexpected AgentVault path: " + path);
            } catch (com.fasterxml.jackson.core.JsonProcessingException error) {
                throw new IllegalStateException(error);
            }
        }

        private ObjectNode intent(boolean settled) {
            ObjectNode value = JSON.createObjectNode();
            value.put("id", "intent-1");
            value.put("scenario_id", "scenario");
            value.put("run_id", "run-1");
            value.put("wallet_id", "wallet-1");
            value.put("policy_id", "policy-1");
            value.put("price_id", "price-1");
            value.put("direction", "inbound");
            value.put("protocol", protocol);
            value.putObject("asset")
                    .put("network", "eip155:42431")
                    .put("asset", "USDC")
                    .put("decimals", 6);
            value.put("recipient", "0x1111111111111111111111111111111111111111");
            value.put("amount", "1000");
            value.put("settled_amount", settled ? "1000" : "0");
            value.put("resource", "mcp://tool/marketpulse.realtime");
            value.put("challenge_hash", "challenge-1");
            value.put("request_hash", requestHashes.get(requestHashes.size() - 1));
            value.put("status", settled ? "settled" : "created");
            value.put("reservation_status", settled ? "consumed" : "unreserved");
            value.put("reserved_amount", "0");
            value.put("tx_hash", settled ? "0x1111" : null);
            value.put("created_at", 1);
            value.put("updated_at", 2);
            if (settled) value.put("settled_at", 2_000_000_000L);
            return value;
        }
    }

    static final class PayerTransport implements VaultTransport {
        private final String protocol;
        private final JsonNode contract;
        private final String resource;
        final List<String> paths = new ArrayList<>();
        JsonNode report;

        PayerTransport(String protocol, JsonNode contract, String resource) {
            this.protocol = protocol;
            this.contract = contract;
            this.resource = resource;
        }

        @Override
        public <T> T get(String path, Class<T> type) {
            throw new UnsupportedOperationException();
        }

        @Override
        public <T> T post(String path, Object body, Class<T> type) {
            paths.add(path);
            try {
                if (path.equals("/api/v1/payments/authorize")) {
                    ObjectNode response = JSON.createObjectNode();
                    response.putObject("authorization")
                            .put("outcome", "authorized")
                            .put("intent_id", "intent-1");
                    ObjectNode intent = intent();
                    response.set("intent", intent);
                    JsonNode meta = contract.path("credential_meta");
                    JsonNode value =
                            protocol.equals("x402")
                                    ? meta.path("x402/payment")
                                    : meta.path("org.paymentauth/credential");
                    String headerValue =
                            protocol.equals("x402")
                                    ? Base64.getEncoder()
                                            .encodeToString(JSON.writeValueAsBytes(value))
                                    : new MppAdapter()
                                            .presentedMcpCredential(meta, 64 * 1024)
                                            .path("authorization")
                                            .asText();
                    ObjectNode credential = response.putObject("credential");
                    credential.put("protocol", protocol);
                    credential.put("challenge_hash", "challenge-1");
                    credential.put("credential_hash", "credential-1");
                    credential.set("payload", value);
                    credential
                            .putObject("http")
                            .put(
                                    "bound_origin",
                                    URI.create(resource).getScheme()
                                            + "://"
                                            + URI.create(resource).getAuthority())
                            .put(
                                    "header_name",
                                    protocol.equals("x402")
                                            ? "Payment-Signature"
                                            : "Payment-Authorization")
                            .put("header_value", headerValue);
                    return JSON.treeToValue(response, type);
                }
                if (path.equals("/api/v1/payments/intent-1/outbound-response")) {
                    report = JSON.valueToTree(body);
                    ObjectNode response = JSON.createObjectNode();
                    response.set("intent", intent());
                    response.put("accepted", true);
                    response.put("replayed", false);
                    return JSON.treeToValue(response, type);
                }
                throw new AssertionError("unexpected AgentVault path: " + path);
            } catch (com.fasterxml.jackson.core.JsonProcessingException error) {
                throw new IllegalStateException(error);
            }
        }

        private ObjectNode intent() {
            ObjectNode value = JSON.createObjectNode();
            value.put("id", "intent-1");
            value.put("scenario_id", "scenario");
            value.put("run_id", "run-1");
            value.put("wallet_id", "wallet-1");
            value.put("policy_id", "policy-1");
            value.put("direction", "outbound");
            value.put("protocol", protocol);
            value.putObject("asset")
                    .put("network", "eip155:42431")
                    .put("asset", "USDC")
                    .put("decimals", 6);
            value.put("recipient", "0x1111111111111111111111111111111111111111");
            value.put("amount", "1000");
            value.put("settled_amount", "0");
            value.put("resource", resource);
            value.put("challenge_hash", "challenge-1");
            value.put("request_hash", "request-1");
            value.put("status", "signed");
            value.put("reservation_status", "reserved");
            value.put("reserved_amount", "1000");
            value.put("created_at", 1);
            value.put("updated_at", 2);
            return value;
        }
    }
}
