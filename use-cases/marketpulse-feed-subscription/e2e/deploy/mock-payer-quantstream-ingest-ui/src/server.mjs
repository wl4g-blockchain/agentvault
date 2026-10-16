import { createServer } from "node:http";
import { randomBytes, randomUUID, timingSafeEqual } from "node:crypto";
import { readFile } from "node:fs/promises";
import { fileURLToPath } from "node:url";
import { dirname, join } from "node:path";
import mqtt from "mqtt";
import {
  ApprovalQueue,
  approvalDecisionBody,
  approvalTopics,
  loadPendingApprovals,
  mqttConnectionOptions
} from "./approval_support.mjs";

const here = dirname(fileURLToPath(import.meta.url));
const indexHtml = await readFile(join(here, "..", "public", "index.html"));
const bindHost = required("QUANTSTREAM_UI_HOST");
const bindPort = positiveInteger("QUANTSTREAM_UI_PORT");
const clientId = required("QUANTSTREAM_CLIENT_ID");
const scenarioId = required("QUANTSTREAM_SCENARIO_ID");
const approverPrincipal = required("QUANTSTREAM_APPROVER_PRINCIPAL");
const adminToken = required("QUANTSTREAM_ADMIN_UI_TOKEN");
const authguardBearer = process.env.AUTHGUARD_APPROVER_BEARER_TOKEN?.trim() ?? "";
const decisionTimeoutMs = positiveInteger("AGENTVAULT_MQTT_TIMEOUT_SECONDS") * 1_000;
const mqttURL = required("AGENTVAULT_MQTT_URL");
const vaultURL = required("AGENTVAULT_URL");
if (!mqttURL.startsWith("mqtt://127.0.0.1:") && !mqttURL.startsWith("mqtt://localhost:") && !mqttURL.startsWith("mqtts://")) {
  throw new Error("plain MQTT is allowed only on loopback");
}

const topics = approvalTopics(clientId, approverPrincipal);
const approvalTopic = topics.required;
const decisionTopic = topics.decision;
const decisionResultPrefix = topics.resultPrefix;
const pending = new ApprovalQueue(scenarioId);
const decisionKeys = new Map();
const decisionAcks = new Map();
const mqttClient = mqtt.connect(mqttURL, mqttConnectionOptions(
  clientId,
  required("AGENTVAULT_MQTT_APPROVER_USERNAME"),
  required("AGENTVAULT_MQTT_APPROVER_PASSWORD")
));
let reconciliation = null;
let lastReconciledAt = 0;

mqttClient.on("connect", () => {
  mqttClient.subscribe([approvalTopic, `${decisionResultPrefix}+`], { qos: 1 }, (error) => {
    if (error) {
      console.error("approval subscription failed");
      return;
    }
    void reconcilePending(true);
  });
});

mqttClient.on("message", (topic, payload) => {
  if (payload.length > 256 * 1024) return;
  if (topic.startsWith(decisionResultPrefix)) {
    const correlationId = topic.slice(decisionResultPrefix.length);
    const waiter = decisionAcks.get(correlationId);
    if (!waiter) return;
    try {
      const envelope = JSON.parse(payload.toString("utf8"));
      if (envelope.correlationId !== correlationId || typeof envelope.status !== "number") return;
      decisionAcks.delete(correlationId);
      waiter.resolve(envelope);
    } catch {
      // A malformed response cannot complete an administrator decision.
    }
    return;
  }
  if (topic !== approvalTopic) return;
  try {
    const approval = JSON.parse(payload.toString("utf8"));
    pending.upsert(approval);
  } catch {
    // Malformed broker input is ignored without echoing sensitive payloads.
  }
});

const server = createServer(async (request, response) => {
  try {
    if (request.method === "GET" && request.url === "/healthz") return send(response, 200, "text/plain", "ok");
    if (request.method === "GET" && request.url === "/") return send(response, 200, "text/html; charset=utf-8", indexHtml);
    if (request.method === "GET" && request.url === "/api/approvals") {
      if (!authorized(request)) return json(response, 401, { error: "unauthorized" });
      await reconcilePending(false);
      return json(response, 200, { items: pending.snapshot() });
    }
    const match = request.url?.match(/^\/api\/approvals\/([A-Za-z0-9._-]+)\/decision$/);
    if (request.method === "POST" && match) {
      if (!authorized(request)) return json(response, 401, { error: "unauthorized" });
      const approval = pending.get(match[1]);
      if (!approval) return json(response, 404, { error: "approval not found" });
      const body = await readJSON(request, 16 * 1024);
      if (body.decision !== "approve" && body.decision !== "reject") {
        return json(response, 400, { error: "decision must be approve or reject" });
      }
      const decisionKey = decisionKeys.get(match[1]) ?? randomUUID();
      decisionKeys.set(match[1], decisionKey);
      const correlationId = randomBytes(16).toString("hex");
      const decisionBody = approvalDecisionBody(approval, body.decision, decisionKey);
      if (typeof body.reason === "string" && body.reason.trim()) decisionBody.reason = body.reason.slice(0, 512);
      const message = {
        type: "payment.approval_decision",
        version: 1,
        correlationId,
        request: {
          method: "POST",
          path: `/api/v1/approvals/${encodeURIComponent(approval.approvalId)}/decision`,
          headers: {
            "content-type": "application/json",
            "idempotency-key": decisionKey
          },
          body: Buffer.from(JSON.stringify(decisionBody), "utf8").toString("base64url")
        }
      };
      const result = await publishDecision(correlationId, JSON.stringify(message));
      const resultBody = decodeResultBody(result.body);
      if (result.status >= 200 && result.status < 300) pending.remove(match[1]);
      return json(response, result.status, resultBody);
    }
    return json(response, 404, { error: "not found" });
  } catch (error) {
    console.error("admin UI request failed");
    return json(response, 500, { error: "internal error" });
  }
});

server.listen(bindPort, bindHost, () => console.log(`QuantStream admin UI ready on ${bindHost}:${bindPort}`));
const reconciliationTimer = setInterval(() => void reconcilePending(false), 5_000);
for (const event of ["SIGINT", "SIGTERM"]) {
  process.on(event, () => {
    clearInterval(reconciliationTimer);
    mqttClient.end(true);
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
        timeoutMs: decisionTimeoutMs
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

function publishDecision(correlationId, payload) {
  return new Promise((resolve, reject) => {
    const timer = setTimeout(() => {
      decisionAcks.delete(correlationId);
      reject(new Error("approval decision timed out"));
    }, decisionTimeoutMs);
    decisionAcks.set(correlationId, {
      resolve: (value) => {
        clearTimeout(timer);
        resolve(value);
      }
    });
    mqttClient.publish(decisionTopic, payload, { qos: 1, retain: false }, (error) => {
      if (!error) return;
      clearTimeout(timer);
      decisionAcks.delete(correlationId);
      reject(error);
    });
  });
}

function decodeResultBody(encoded) {
  try {
    return JSON.parse(Buffer.from(encoded, "base64url").toString("utf8"));
  } catch {
    return { error: "invalid AgentVault approval response" };
  }
}

function send(response, status, contentType, body) {
  response.writeHead(status, { "content-type": contentType, "cache-control": "no-store" });
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
  if (!Number.isSafeInteger(value) || value <= 0 || value > 65_535) throw new Error(`${name} is invalid`);
  return value;
}
