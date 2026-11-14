import type { HttpConfig } from "../config/config.js";
import {
  ApiError,
  readBounded,
  type OwnedVaultTransport,
} from "./transport.js";
import { randomBytes } from "node:crypto";
export class HttpVaultTransport implements OwnedVaultTransport {
  private closed = false;
  private readonly pending = new Set<AbortController>();
  constructor(private readonly config: HttpConfig) {}
  async request<T>(
    method: "GET" | "POST",
    path: string,
    body?: unknown,
  ): Promise<T> {
    if (this.closed) throw new Error("AgentVault HTTP transport is closed");
    if (!path.startsWith("/api/v1/") || path.includes("\\"))
      throw new Error("invalid AgentVault HTTP request");
    const payload = body === undefined ? undefined : JSON.stringify(body);
    if (
      payload !== undefined &&
      Buffer.byteLength(payload) > this.config.maxResponseBytes
    )
      throw new Error("AgentVault HTTP request exceeds configured limit");
    const controller = new AbortController();
    this.pending.add(controller);
    const timer = setTimeout(() => controller.abort(), this.config.timeoutMs);
    const traceId = randomBytes(16).toString("hex"),
      spanId = randomBytes(8).toString("hex");
    console.info(
      JSON.stringify({
        event_name: "agentvault.sdk.transport.http.produced",
        trace_id: traceId,
        span_id: spanId,
        parent_id: "",
      }),
    );
    const headers: Record<string, string> = {
      accept: "application/json",
      traceparent: `00-${traceId}-${spanId}-01`,
    };
    if (payload !== undefined) headers["content-type"] = "application/json";
    if (this.config.authguardBearer)
      headers["authorization"] = `Bearer ${this.config.authguardBearer}`;
    try {
      const response = await fetch(
        this.config.baseUrl.replace(/\/$/, "") + path,
        {
          method,
          headers,
          ...(payload === undefined ? {} : { body: payload }),
          redirect: "manual",
          signal: controller.signal,
        },
      );
      const raw = await readBounded(response, this.config.maxResponseBytes);
      let value: unknown;
      try {
        value = JSON.parse(new TextDecoder().decode(raw));
      } catch {
        if (response.ok) throw new Error("invalid AgentVault HTTP response");
      }
      if (!response.ok) {
        const detail = (
          value as { error?: { code?: string; message?: string } } | undefined
        )?.error;
        throw new ApiError(
          response.status,
          detail?.code ?? "TRANSPORT_ERROR",
          detail?.message ?? "AgentVault HTTP request failed",
        );
      }
      return value as T;
    } finally {
      clearTimeout(timer);
      this.pending.delete(controller);
    }
  }
  close(): void {
    this.closed = true;
    for (const request of this.pending) request.abort();
    this.pending.clear();
  }
}
