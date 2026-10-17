import { isIP } from "node:net";

const MAX_AMOUNT = (1n << 128n) - 1n;
const MAX_PAGE_BYTES = 256 * 1024;
const DEFAULT_PAGE_SIZE = 100;

export function approvalDecisionBody(approval, decision) {
  if (decision !== "approve" && decision !== "reject") {
    throw new Error("decision must be approve or reject");
  }
  const normalized = normalizeApproval(approval, approval.scenarioId);
  const body = {
    scenario_id: normalized.scenarioId,
    run_id: normalized.runId,
    approval_id: normalized.approvalId,
    decision,
    reason: "QuantStream administrator decision"
  };
  if (decision === "approve") body.approved_amount = decimalAmount(normalized.requestedAmount);
  return body;
}

export function mqttConnectionOptions(clientId, username, password) {
  return {
    clientId: `quantstream-admin-${requiredIdentifier(clientId, "client id")}`,
    username: requiredText(username, "MQTT username"),
    password: requiredText(password, "MQTT password"),
    clean: false,
    reconnectPeriod: 1_000,
    connectTimeout: 5_000
  };
}

export function approvalTopics(clientId, approverPrincipal) {
  const client = requiredIdentifier(clientId, "client id");
  const approver = requiredIdentifier(approverPrincipal, "approver principal");
  const root = `agentvault/v1/clients/${client}`;
  return {
    required: `${root}/approvals/required`,
    decision: `${root}/approvers/${approver}/decisions`,
    resultPrefix: `${root}/approvers/${approver}/results/`
  };
}

export class ApprovalQueue {
  constructor(scenarioId) {
    this.scenarioId = requiredIdentifier(scenarioId, "scenario id");
    this.items = new Map();
    this.revision = 0;
    this.operations = [];
  }

  snapshot() {
    return [...this.items.values()].sort((left, right) => left.approvalId.localeCompare(right.approvalId));
  }

  get(approvalId) {
    return this.items.get(requiredIdentifier(approvalId, "approval id"));
  }

  beginReconcile() {
    return this.revision;
  }

  upsert(raw) {
    const approval = normalizeApproval(raw, this.scenarioId);
    const existing = this.items.get(approval.approvalId);
    if (existing) {
      if (!sameApproval(existing, approval)) throw new Error("approval notification changed immutable fields");
      return false;
    }
    this.items.set(approval.approvalId, approval);
    this.record({ type: "upsert", approval });
    return true;
  }

  remove(approvalId) {
    const id = requiredIdentifier(approvalId, "approval id");
    if (!this.items.delete(id)) return false;
    this.record({ type: "remove", approvalId: id });
    return true;
  }

  replaceFromAuthority(rawItems, startedAtRevision) {
    if (!Number.isSafeInteger(startedAtRevision) || startedAtRevision < 0 || startedAtRevision > this.revision) {
      throw new Error("invalid approval reconciliation revision");
    }
    const authoritative = new Map();
    for (const raw of rawItems) {
      const approval = normalizeApproval(raw, this.scenarioId);
      const existing = authoritative.get(approval.approvalId);
      if (existing && !sameApproval(existing, approval)) throw new Error("authority returned conflicting approvals");
      authoritative.set(approval.approvalId, approval);
    }
    for (const operation of this.operations) {
      if (operation.revision <= startedAtRevision) continue;
      if (operation.type === "upsert") authoritative.set(operation.approval.approvalId, operation.approval);
      else authoritative.delete(operation.approvalId);
    }
    this.items = authoritative;
    this.operations = this.operations.filter((operation) => operation.revision > startedAtRevision);
  }

  record(operation) {
    this.revision += 1;
    this.operations.push({ ...operation, revision: this.revision });
  }
}

export async function loadPendingApprovals({
  baseUrl,
  scenarioId,
  authguardBearer = "",
  fetchImpl = fetch,
  timeoutMs = 5_000,
  pageSize = DEFAULT_PAGE_SIZE
}) {
  const scenario = requiredIdentifier(scenarioId, "scenario id");
  if (authguardBearer && /[\r\n]/.test(authguardBearer)) throw new Error("AuthGuard bearer credential is invalid");
  if (!Number.isSafeInteger(timeoutMs) || timeoutMs <= 0 || timeoutMs > 60_000) {
    throw new Error("approval API timeout is invalid");
  }
  if (!Number.isSafeInteger(pageSize) || pageSize <= 0 || pageSize > DEFAULT_PAGE_SIZE) {
    throw new Error("approval API page size is invalid");
  }

  const approvals = new Map();
  let offset = 0;
  let total = 1;
  while (offset < total) {
    const url = approvalPageUrl(baseUrl, scenario, offset, pageSize);
    const headers = { accept: "application/json" };
    if (authguardBearer) headers.authorization = `Bearer ${authguardBearer}`;
    const response = await fetchImpl(url, {
      method: "GET",
      headers,
      cache: "no-store",
      redirect: "error",
      signal: AbortSignal.timeout(timeoutMs)
    });
    if (!response.ok) throw new Error(`AgentVault pending approval query failed (${response.status})`);
    const page = await boundedJson(response, MAX_PAGE_BYTES);
    if (!page || !Array.isArray(page.items) || !Number.isSafeInteger(page.total) || page.total < 0) {
      throw new Error("AgentVault returned an invalid approval page");
    }
    total = page.total;
    if (page.items.length === 0 && offset < total) {
      throw new Error("AgentVault approval pagination made no progress");
    }
    for (const raw of page.items) {
      const approval = normalizeApproval(raw, scenario);
      const existing = approvals.get(approval.approvalId);
      if (existing && !sameApproval(existing, approval)) throw new Error("AgentVault returned conflicting approvals");
      approvals.set(approval.approvalId, approval);
    }
    offset += page.items.length;
    if (approvals.size > 10_000 || offset > 10_000) throw new Error("AgentVault approval queue is unexpectedly large");
  }
  return [...approvals.values()];
}

export function normalizeApproval(raw, expectedScenarioId) {
  if (!raw || typeof raw !== "object" || Array.isArray(raw)) throw new Error("approval is invalid");
  if (raw.type !== undefined && (raw.type !== "payment.approval_required" || raw.version !== 1)) {
    throw new Error("approval notification contract is invalid");
  }
  const scenarioId = requiredIdentifier(raw.scenarioId ?? raw.scenario_id ?? expectedScenarioId, "scenario id");
  if (scenarioId !== expectedScenarioId) throw new Error("approval belongs to another scenario");
  const expiresAt = raw.expiresAt ?? raw.expires_at;
  if (!Number.isSafeInteger(expiresAt) || expiresAt <= 0) throw new Error("approval expiry is invalid");
  return {
    approvalId: requiredIdentifier(raw.approvalId ?? raw.id, "approval id"),
    intentId: requiredIdentifier(raw.intentId ?? raw.intent_id, "intent id"),
    runId: requiredIdentifier(raw.runId ?? raw.run_id, "run id"),
    scenarioId,
    requestedAmount: decimalAmount(raw.requestedAmount ?? raw.requested_amount),
    expiresAt
  };
}

function approvalPageUrl(baseUrl, scenarioId, offset, limit) {
  const base = trustedBaseUrl(baseUrl);
  const url = new URL("approvals", base);
  url.searchParams.set("scenario_id", scenarioId);
  url.searchParams.set("status", "pending");
  url.searchParams.set("offset", String(offset));
  url.searchParams.set("limit", String(limit));
  return url;
}

function trustedBaseUrl(value) {
  const url = new URL(requiredText(value, "AgentVault URL"));
  const loopback = url.hostname === "localhost" || isLoopbackIp(url.hostname);
  if ((url.protocol !== "https:" && !(url.protocol === "http:" && loopback)) || url.username || url.password) {
    throw new Error("AgentVault URL must use HTTPS or loopback HTTP without embedded credentials");
  }
  if (url.search || url.hash) throw new Error("AgentVault URL cannot contain a query or fragment");
  if (!url.pathname.endsWith("/")) url.pathname += "/";
  return url;
}

function isLoopbackIp(hostname) {
  const normalized = hostname.replace(/^\[|\]$/g, "");
  const family = isIP(normalized);
  if (family === 4) return normalized.startsWith("127.");
  if (family === 6) return normalized === "::1";
  return false;
}

async function boundedJson(response, limit) {
  const contentLength = response.headers.get("content-length");
  if (contentLength !== null) {
    const length = Number.parseInt(contentLength, 10);
    if (!Number.isSafeInteger(length) || length < 0 || length > limit) throw new Error("approval API response is too large");
  }
  if (!response.body) throw new Error("approval API response has no body");
  const reader = response.body.getReader();
  const chunks = [];
  let size = 0;
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    size += value.byteLength;
    if (size > limit) {
      await reader.cancel();
      throw new Error("approval API response is too large");
    }
    chunks.push(value);
  }
  const bytes = new Uint8Array(size);
  let cursor = 0;
  for (const chunk of chunks) {
    bytes.set(chunk, cursor);
    cursor += chunk.byteLength;
  }
  try {
    return JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(bytes));
  } catch {
    throw new Error("approval API response is not valid JSON");
  }
}

function decimalAmount(value) {
  if (typeof value !== "string" || !/^(0|[1-9][0-9]*)$/.test(value)) throw new Error("approval amount is invalid");
  const amount = BigInt(value);
  if (amount <= 0n || amount > MAX_AMOUNT) throw new Error("approval amount is invalid");
  return value;
}

function requiredIdentifier(value, field) {
  const text = requiredText(value, field);
  if (text.length > 128 || !/^[A-Za-z0-9._:-]+$/.test(text)) throw new Error(`${field} is invalid`);
  return text;
}

function requiredText(value, field) {
  if (typeof value !== "string" || !value.trim()) throw new Error(`${field} is required`);
  return value;
}

function sameApproval(left, right) {
  return left.approvalId === right.approvalId
    && left.intentId === right.intentId
    && left.runId === right.runId
    && left.scenarioId === right.scenarioId
    && left.requestedAmount === right.requestedAmount
    && left.expiresAt === right.expiresAt;
}
