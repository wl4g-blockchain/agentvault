import assert from "node:assert/strict";
import { createServer } from "node:http";
import { after, before, test } from "node:test";

import {
  ApprovalQueue,
  approvalDecisionBody,
  loadPendingApprovals,
} from "../../../../deploy/mock-payer-quantstream-ui/source/src/approval_support.mjs";

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
      url: request.url,
    });
    const url = new URL(request.url, "http://localhost");
    const offset = Number.parseInt(url.searchParams.get("offset"), 10);
    const limit = Number.parseInt(url.searchParams.get("limit"), 10);
    const items = authority.slice(offset, offset + limit).map((item) => ({
      id: item.approvalId,
      intent_id: item.intentId,
      run_id: item.runId,
      requested_amount: item.requestedAmount,
      expires_at: item.expiresAt,
      status: "pending",
    }));
    response.writeHead(200, { "content-type": "application/json" });
    response.end(JSON.stringify({ items, total: authority.length }));
  });
  await new Promise((resolve) => server.listen(0, "127.0.0.1", resolve));
  const address = server.address();
  baseUrl = `http://127.0.0.1:${address.port}/api/v1`;
});

after(async () => {
  await new Promise((resolve, reject) =>
    server.close((error) => (error ? reject(error) : resolve())),
  );
});

test("approve binds the exact requested amount while reject omits it", () => {
  assert.deepEqual(approvalDecisionBody(first, "approve"), {
    scenario_id: scenarioId,
    run_id: "run-approval-1",
    approval_id: "approval-1",
    decision: "approve",
    reason: "QuantStream administrator decision",
    approved_amount: "1000000",
  });
  assert.equal(
    "approved_amount" in approvalDecisionBody(first, "reject"),
    false,
  );
  assert.throws(
    () => approvalDecisionBody({ ...first, requestedAmount: "1e6" }, "approve"),
    /amount is invalid/,
  );
  assert.throws(
    () =>
      approvalDecisionBody(
        { ...first, requestedAmount: (1n << 128n).toString() },
        "approve",
      ),
    /amount is invalid/,
  );
});

test("approval API refuses credential-bearing redirects and non-loopback plaintext URLs", async () => {
  await assert.rejects(
    loadPendingApprovals({
      baseUrl: "http://example.com/api/v1",
      scenarioId,
      authguardBearer: "approver-token",
      fetchImpl: () => assert.fail("unsafe URL must fail before fetch"),
    }),
    /HTTPS or loopback HTTP/,
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
    },
  });
  assert.equal(redirectPolicy, "error");
});

function approval(approvalId, intentId, requestedAmount, expiresAt) {
  return {
    approvalId,
    intentId,
    runId: `run-${approvalId}`,
    scenarioId,
    requestedAmount,
    expiresAt,
  };
}
