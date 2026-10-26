import { A2ACall } from '../model/a2a.js';
import { readBounded } from '../transport/transport.js';
import type { PayerRequestContext } from '../model/payer.js';
import { PayerMiddleware, safeHttpUrl } from './payer.js';
import type { AuthorizedPayment } from '../model/payer.js';

interface McpCall {
  readonly id: string | number;
  readonly message: Record<string, unknown>;
  readonly body: Uint8Array;
  readonly a2a?: A2ACall;
}

/** Native Fetch payer adapter with transparent HTTP and MCP payment handling. */
export class FetchPayerMiddleware {
  private readonly payments = new WeakMap<Response, AuthorizedPayment>();

  constructor(
    private readonly core: PayerMiddleware,
    private readonly fetchImpl: typeof globalThis.fetch = globalThis.fetch,
  ) {}

  payment(response: Response): AuthorizedPayment | undefined {
    return this.payments.get(response);
  }

  async request(
    input: URL | RequestInfo,
    init?: RequestInit,
    operation?: PayerRequestContext,
  ): Promise<Response> {
    const source = new Request(input, init);
    if (!operation) return this.fetchImpl(source);
    const request = new Request(source, { redirect: 'manual' });
    const target = safeHttpUrl(request.url);
    const replay = request.clone();
    const mcpCall = await this.mcpCall(replay.clone());
    if (mcpCall?.a2a) {
      const existing = request.headers.get('x-run-id');
      if (existing !== null && existing !== operation.runId) throw new Error('A2A run_id conflicts with paid operation');
      for (const outgoing of [request, replay]) {
        outgoing.headers.set('x-run-id', operation.runId);
        outgoing.headers.set('x-a2a-extensions', this.core.a2a().extensions(outgoing.headers.get('x-a2a-extensions') ?? ''));
      }
    }
    const first = await this.fetchImpl(request);
    if (first.status !== 402) {
      const paid = await this.handleMcpChallenge(
        first,
        replay,
        target,
        mcpCall,
        operation,
      );
      return paid ?? first;
    }
    sdkEvent('agentvault.sdk.payer.payment_required', { runId: operation.runId });
    const challengeBody = await readBounded(first.clone(), this.core.config.maxChallengeBytes);
    sdkEvent('agentvault.sdk.payer.signing_requested', { runId: operation.runId });
    const authorized = await this.core.authorize(
      {
        method: replay.method,
        url: target.href,
        origin: target.origin,
        headers: replay.headers,
        followRedirects: false,
      },
      { status: first.status, headers: first.headers, body: challengeBody },
      operation,
    );
    if (!authorized) return first;
    sdkEvent('agentvault.sdk.payer.signed', {
      runId: operation.runId,
      intentId: authorized.intent.id,
    });
    const [headerName, headerValue] = this.core.credentialHeader(
      authorized,
      target.origin,
      replay.headers,
    );
    const headers = new Headers(replay.headers);
    headers.set(headerName, headerValue);
    const response = await this.core.deliver(operation, authorized, {
      send: async () => {
        const paid = new Request(replay.clone(), { headers, redirect: 'manual' });
        return this.fetchImpl(paid);
      },
      view: (value) => ({ status: value.status, headers: value.headers }),
      discard: async (value) => {
        if (value.body) await value.body.cancel();
      },
    });
    this.payments.set(response, authorized);
    return response;
  }

  private async handleMcpChallenge(
    first: Response,
    replay: Request,
    target: URL,
    call: McpCall | undefined,
    operation: PayerRequestContext,
  ): Promise<Response | undefined> {
    if (!call || !this.isJson(first.headers.get('content-type'))) return undefined;
    const firstBody = await readBounded(first.clone(), this.core.config.maxChallengeBytes);
    let message: unknown;
    try {
      message = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(firstBody));
    } catch {
      return undefined;
    }
    const authorize = call.a2a ? this.core.a2a().authorize.bind(this.core.a2a()) : this.core.authorizeMcp.bind(this.core);
    const authorized = await authorize(
      {
        method: replay.method,
        url: target.href,
        origin: target.origin,
        headers: replay.headers,
        body: call.body,
        followRedirects: false,
      },
      message,
      operation,
      first.headers,
    );
    if (!authorized) return undefined;
    sdkEvent('agentvault.sdk.payer.payment_required', { runId: operation.runId });
    sdkEvent('agentvault.sdk.payer.signed', {
      runId: operation.runId,
      intentId: authorized.intent.id,
    });
    const metadata = call.a2a ? undefined : this.core.mcpCredential(authorized, target.origin);
    const paidBody = call.a2a ? this.core.a2a().credential(authorized, call.a2a, message, target.origin)
      : this.attachMcpCredential(call.message, metadata!.key, metadata!.value);
    const response = await this.core.deliver(operation, authorized, {
      send: async () => this.fetchImpl(new Request(replay.clone(), {
        body: paidBody,
        headers: replay.headers,
        redirect: 'manual',
      })),
      view: (value) => ({ status: value.status, headers: value.headers }),
      discard: async (value) => {
        if (value.body) await value.body.cancel();
      },
      report: async (value) => {
        if (value.status === 503 || value.status < 200 || value.status >= 300) return;
        if (!this.isJson(value.headers.get('content-type'))) {
          throw new Error('paid MCP response must be application/json');
        }
        const body = await readBounded(value.clone(), this.core.config.maxChallengeBytes);
        let paidMessage: unknown;
        try {
          paidMessage = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(body));
        } catch {
          throw new Error('paid MCP response is not valid JSON');
        }
        if (call.a2a) {
          await this.core.a2a().report(operation, authorized, call.a2a, paidMessage, value.headers);
          return;
        }
        if (!this.isRecord(paidMessage) || paidMessage.jsonrpc !== '2.0' || paidMessage.id !== call.id ||
            !this.isRecord(paidMessage.result)) {
          throw new Error('paid MCP response has no matching JSON-RPC result');
        }
        await this.core.reportMcp(
          operation,
          authorized,
          this.isRecord(paidMessage.result._meta) ? paidMessage.result._meta : {},
        );
      },
    });
    this.payments.set(response, authorized);
    return response;
  }

  private async mcpCall(request: Request): Promise<McpCall | undefined> {
    if (request.method.toUpperCase() !== 'POST' ||
        !this.isJson(request.headers.get('content-type'))) return undefined;
    const body = await this.readRequestBody(request, this.core.config.maxChallengeBytes);
    let message: unknown;
    try {
      message = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(body));
    } catch {
      return undefined;
    }
    const a2a = A2ACall.parse(body, this.core.config.maxChallengeBytes);
    if (a2a) return { id: a2a.envelope.id, message: a2a.wire, body, a2a };
    if (!this.isRecord(message) || message.jsonrpc !== '2.0' ||
        message.method !== 'tools/call' || !this.isRecord(message.params) ||
        (typeof message.id !== 'string' && typeof message.id !== 'number')) return undefined;
    return { id: message.id, message, body };
  }

  private attachMcpCredential(
    message: Record<string, unknown>,
    key: string,
    value: unknown,
  ): string {
    const copy = structuredClone(message);
    if (!this.isRecord(copy.params)) throw new Error('MCP params must be an object');
    const rootMeta = copy._meta ?? {};
    if (!this.isRecord(rootMeta)) throw new Error('JSON-RPC root _meta must be an object');
    if (this.core.mcpRootRequestMetadataKeys().some((candidate) => candidate in rootMeta)) {
      throw new Error('MCP request already contains root payment credentials');
    }
    const meta = copy.params._meta ?? {};
    if (!this.isRecord(meta)) throw new Error('MCP params._meta must be an object');
    if (this.core.mcpRequestMetadataKeys().some((candidate) => candidate in meta)) {
      throw new Error('MCP request already contains payment credentials');
    }
    copy.params._meta = { ...meta, [key]: value };
    return JSON.stringify(copy);
  }

  private async readRequestBody(request: Request, maximum: number): Promise<Uint8Array> {
    const declared = request.headers.get('content-length');
    if (declared !== null && Number(declared) > maximum) {
      throw new Error('paid request body exceeds configured limit');
    }
    if (!request.body) return new Uint8Array();
    const reader = request.body.getReader();
    const chunks: Uint8Array[] = [];
    let length = 0;
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        length += value.length;
        if (length > maximum) {
          await reader.cancel();
          throw new Error('paid request body exceeds configured limit');
        }
        chunks.push(value);
      }
    } finally {
      reader.releaseLock();
    }
    const result = new Uint8Array(length);
    let offset = 0;
    for (const chunk of chunks) {
      result.set(chunk, offset);
      offset += chunk.length;
    }
    return result;
  }

  private isJson(value: string | null): boolean {
    return value?.toLowerCase().startsWith('application/json') === true;
  }

  private isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
  }
}

function sdkEvent(event: string, fields: Record<string, unknown>): void {
  console.info(JSON.stringify({ event, ...fields }));
}
