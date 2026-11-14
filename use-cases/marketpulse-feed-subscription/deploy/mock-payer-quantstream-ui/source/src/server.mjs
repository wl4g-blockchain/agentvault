import { createServer } from "node:http";
import { randomBytes, timingSafeEqual } from "node:crypto";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import {
  ApprovalQueue,
  approvalDecisionBody,
  loadPendingApprovals,
  submitApprovalDecision,
} from "./approval_support.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const indexHtml = await readFile(join(here, "..", "public", "index.html"));
const bindHost = required("QUANTSTREAM_UI_HOST");
const bindPort = positiveInteger("QUANTSTREAM_UI_PORT");
const clientId = required("QUANTSTREAM_CLIENT_ID");
const scenarioId = required("QUANTSTREAM_SCENARIO_ID");
const approverPrincipal = required("QUANTSTREAM_APPROVER_PRINCIPAL");
const adminToken = required("QUANTSTREAM_ADMIN_UI_TOKEN");
const authguardBearer =
  process.env.AUTHGUARD_APPROVER_BEARER_TOKEN?.trim() ?? "";
const decisionTimeoutMs =
  positiveInteger("AGENTVAULT_HTTP_TIMEOUT_SECONDS") * 1_000;
const vaultURL = required("AGENTVAULT_URL");
const pending = new ApprovalQueue(scenarioId);
let reconciliation = null;
let lastReconciledAt = 0;

const server = createServer(async (request, response) => {
  try {
    if (request.method === "GET" && request.url === "/healthz")
      return send(response, 200, "text/plain", "ok");
    if (request.method === "GET" && request.url === "/")
      return send(response, 200, "text/html; charset=utf-8", indexHtml);
    if (request.method === "GET" && request.url === "/api/approvals") {
      if (!authorized(request))
        return json(response, 401, { error: "unauthorized" });
      await reconcilePending(false);
      return json(response, 200, { items: pending.snapshot() });
    }
    const match = request.url?.match(
      /^\/api\/approvals\/([A-Za-z0-9._-]+)\/decision$/,
    );
    if (request.method === "POST" && match) {
      if (!authorized(request))
        return json(response, 401, { error: "unauthorized" });
      const approval = pending.get(match[1]);
      if (!approval)
        return json(response, 404, { error: "approval not found" });
      const body = await readJSON(request, 16 * 1024);
      if (body.decision !== "approve" && body.decision !== "reject") {
        return json(response, 400, {
          error: "decision must be approve or reject",
        });
      }
      const traceId = randomNonzeroHex(16);
      const requestSpanId = randomNonzeroHex(8);
      const traceparent = `00-${traceId}-${requestSpanId}-01`;
      const decisionBody = approvalDecisionBody(approval, body.decision);
      if (typeof body.reason === "string" && body.reason.trim())
        decisionBody.reason = body.reason.slice(0, 512);
      const result = await submitApprovalDecision({
        baseUrl: vaultURL,
        approval,
        decision: body.decision,
        reason: decisionBody.reason,
        authguardBearer,
        traceparent,
        timeoutMs: decisionTimeoutMs,
      });
      const resultBody = result.body;
      if (result.status >= 200 && result.status < 300) pending.remove(match[1]);
      return json(response, result.status, resultBody);
    }
    return json(response, 404, { error: "not found" });
  } catch (error) {
    console.error("admin UI request failed");
    return json(response, 500, { error: "internal error" });
  }
});

server.listen(bindPort, bindHost, () =>
  console.log(`QuantStream admin UI ready on ${bindHost}:${bindPort}`),
);
void reconcilePending(true);
const reconciliationTimer = setInterval(
  () => void reconcilePending(false),
  5_000,
);
for (const event of ["SIGINT", "SIGTERM"]) {
  process.on(event, () => {
    clearInterval(reconciliationTimer);
    server.close(() => process.exit(0));
  });
}

async function reconcilePending(force) {
  if (reconciliation) return reconciliation;
  if (!force && Date.now() - lastReconciledAt < 5_000) return;
  const startedAt = pending.beginReconcile();
  reconciliation = (async () => {
    try {
      const approvals = await loadPendingApprovals({
        baseUrl: vaultURL,
        scenarioId,
        authguardBearer,
        timeoutMs: decisionTimeoutMs,
      });
      pending.replaceFromAuthority(approvals, startedAt);
      lastReconciledAt = Date.now();
    } catch {
      console.error("pending approval reconciliation failed");
    } finally {
      reconciliation = null;
    }
  })();
  return reconciliation;
}

function authorized(request) {
  const supplied = request.headers.authorization?.replace(/^Bearer /, "") ?? "";
  const left = Buffer.from(supplied);
  const right = Buffer.from(adminToken);
  return left.length === right.length && timingSafeEqual(left, right);
}

async function readJSON(request, limit) {
  const chunks = [];
  let size = 0;
  for await (const chunk of request) {
    size += chunk.length;
    if (size > limit) throw new Error("request too large");
    chunks.push(chunk);
  }
  return JSON.parse(Buffer.concat(chunks).toString("utf8"));
}

function randomNonzeroHex(bytes) {
  for (;;) {
    const value = randomBytes(bytes).toString("hex");
    if (!/^0+$/.test(value)) return value;
  }
}

function send(response, status, contentType, body) {
  response.writeHead(status, {
    "content-type": contentType,
    "cache-control": "no-store",
  });
  response.end(body);
}

function json(response, status, body) {
  send(response, status, "application/json", JSON.stringify(body));
}

function required(name) {
  const value = process.env[name];
  if (!value?.trim()) throw new Error(`${name} is required`);
  return value;
}

function positiveInteger(name) {
  const value = Number.parseInt(required(name), 10);
  if (!Number.isSafeInteger(value) || value <= 0 || value > 65_535)
    throw new Error(`${name} is invalid`);
  return value;
}
