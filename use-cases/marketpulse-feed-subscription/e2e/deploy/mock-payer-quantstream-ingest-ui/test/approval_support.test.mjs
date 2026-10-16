import assert from "node:assert/strict";
import { createServer } from "node:http";
import { after, before, test } from "node:test";

import {
  ApprovalQueue,
  approvalDecisionBody,
  approvalTopics,
  loadPendingApprovals,
  mqttConnectionOptions
} from "../src/approval_support.mjs";

const scenarioId = "scenario-quantstream-market-ingestion";
const first = approval("approval-1", "intent-1", "1000000", 2_000_000_000);
const second = approval("approval-2", "intent-2", "50000000", 2_000_000_100);

let authority = [first, second];
let server;
let baseUrl;
const requests = [];

before(async () => {
  server = createServer((request, response) => {
    requests.push({
      authorization: request.headers.authorization,
      url: request.url
    });
    const url = new URL(request.url, "http://localhost");
    const offset = Number.parseInt(url.searchParams.get("offset"), 10);
    const limit = Number.parseInt(url.searchParams.get("limit"), 10);
    const items = authority.slice(offset, offset + limit).map((item) => ({
      id: item.approvalId,
      intent_id: item.intentId,
      requested_amount: item.requestedAmount,
      expires_at: item.expiresAt,
      status: "pending"
    }));
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ items, total: authority.length }));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  baseUrl = `http://127.0.0.1:${address.port}/api/v1`;
});

after(async () => {
  await new Promise((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
});

test("approve binds the exact requested amount while reject omits it", () => {
  assert.deepEqual(approvalDecisionBody(first, "approve", "decision-key-1"), {
    scenario_id: scenarioId,
    idempotency_key: "decision-key-1",
    approval_id: "approval-1",
    decision: "approve",
    reason: "QuantStream administrator decision",
    approved_amount: "1000000"
  });
  assert.equal("approved_amount" in approvalDecisionBody(first, "reject", "decision-key-2"), false);
  assert.throws(
    () => approvalDecisionBody({ ...first, requestedAmount: "1e6" }, "approve", "decision-key-3"),
    /amount is invalid/
  );
  assert.throws(
    () => approvalDecisionBody({ ...first, requestedAmount: (1n << 128n).toString() }, "approve", "decision-key-4"),
    /amount is invalid/
  );
});

test("MQTT identity is stable and requests a persistent broker session", () => {
  const options = mqttConnectionOptions("client-quantstream", "approver", "secret");
  assert.equal(options.clientId, "quantstream-admin-client-quantstream");
  assert.equal(options.clean, false);
  assert.equal(options.password, "secret");
});

test("approval topics bind both the business client and durable approver principal", () => {
  assert.deepEqual(approvalTopics("client-quantstream", "principal-quantstream-admin"), {
    required: "agentvault/v1/clients/client-quantstream/approvals/required",
    decision: "agentvault/v1/clients/client-quantstream/approvers/principal-quantstream-admin/decisions",
    resultPrefix: "agentvault/v1/clients/client-quantstream/approvers/principal-quantstream-admin/results/"
  });
  assert.throws(
    () => approvalTopics("client-quantstream", "principal/admin"),
    /approver principal is invalid/
  );
});

test("offline recovery pages through authoritative pending approvals and deduplicates MQTT replay", async () => {
  requests.length = 0;
  authority = [first, second];
  const recovered = await loadPendingApprovals({
    baseUrl,
    scenarioId,
    authguardBearer: "approver-token",
    pageSize: 1
  });
  assert.deepEqual(recovered, [first, second]);
  assert.equal(requests.length, 2);
  assert.equal(requests[0].authorization, "Bearer approver-token");
  assert.match(requests[0].url, /scenario_id=scenario-quantstream-market-ingestion/);
  assert.match(requests[0].url, /offset=0/);
  assert.match(requests[1].url, /offset=1/);

  const queue = new ApprovalQueue(scenarioId);
  const reconciliationRevision = queue.beginReconcile();
  assert.equal(queue.upsert(first), true);
  assert.equal(queue.upsert(first), false);
  queue.replaceFromAuthority(recovered, reconciliationRevision);
  assert.deepEqual(queue.snapshot(), [first, second]);

  authority = [second];
  const nextRevision = queue.beginReconcile();
  const current = await loadPendingApprovals({ baseUrl, scenarioId, authguardBearer: "approver-token", pageSize: 1 });
  queue.replaceFromAuthority(current, nextRevision);
  assert.deepEqual(queue.snapshot(), [second]);
});

test("approval API refuses credential-bearing redirects and non-loopback plaintext URLs", async () => {
  await assert.rejects(
    loadPendingApprovals({
      baseUrl: "http://example.com/api/v1",
      scenarioId,
      authguardBearer: "approver-token",
      fetchImpl: () => assert.fail("unsafe URL must fail before fetch")
    }),
    /HTTPS or loopback HTTP/
  );

  let redirectPolicy;
  await loadPendingApprovals({
    baseUrl,
    scenarioId,
    authguardBearer: "approver-token",
    pageSize: 1,
    fetchImpl: async (url, options) => {
      redirectPolicy = options.redirect;
      return fetch(url, options);
    }
  });
  assert.equal(redirectPolicy, "error");
});

function approval(approvalId, intentId, requestedAmount, expiresAt) {
  return { approvalId, intentId, scenarioId, requestedAmount, expiresAt };
}
