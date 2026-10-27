import { JSONRPCResponseSchema } from '@modelcontextprotocol/sdk/types.js';
import type { TaskStore, ServerCallContext } from '@a2a-js/sdk/server';
import { A2APaymentCall } from './a2a.js';
import type { PayeeOperation, SettledIncomingPayment } from '../model/payee.js';
import { createHash } from 'node:crypto';
import express, { type RequestHandler } from 'express';
import type { PaymentChallenge } from '../model/base.js';
import type { McpMetadata, PaymentProtocol } from '../protocol/adapter.js';
import { ApiError } from '../transport/transport.js';
import { InvalidPaymentError, type PayeeMiddleware } from './payee.js';

export interface ExpressPayeeRoute {
  method: string;
  path: string;
  scenarioId: string;
  priceId: string;
  resource: string;
  origin: string;
  protocol?: string;
  mcpTool?: string;
  a2aTaskStore?: TaskStore;
}

interface McpCall {
  readonly id: string | number;
  readonly meta: Record<string, unknown>;
  readonly canonicalBody: Buffer;
}

export function mcpToolRoute(
  path: string,
  tool: string,
  scenarioId: string,
  priceId: string,
  resource: string,
  origin: string,
): ExpressPayeeRoute {
  return mcpToolRouteWithProtocol(
    path,
    tool,
    scenarioId,
    priceId,
    resource,
    origin,
    'x402',
  );
}

export function mcpToolRouteWithProtocol(
  path: string,
  tool: string,
  scenarioId: string,
  priceId: string,
  resource: string,
  origin: string,
  protocol: string,
): ExpressPayeeRoute {
  return { method: 'POST', path, scenarioId, priceId, resource, origin, protocol, mcpTool: tool };
}

/** Express middleware that protects only explicitly registered routes. */
export class ExpressPayeeMiddleware {
  constructor(
    private readonly core: PayeeMiddleware,
    private readonly routes: readonly ExpressPayeeRoute[],
    private readonly maxBodyBytes = 1024 * 1024,
  ) {
    if (maxBodyBytes <= 0) throw new Error('maxBodyBytes must be positive');
    if (routes.some((route) => route.mcpTool && route.a2aTaskStore)) throw new Error('MCP and A2A routes are mutually exclusive');
  }

  handlers(): RequestHandler[] {
    return [express.raw({ type: '*/*', limit: this.maxBodyBytes }), this.handler()];
  }

  handler(): RequestHandler {
    return async (request, response, next) => {
      const route = this.routes.find((candidate) =>
        candidate.method.toUpperCase() === request.method && candidate.path === request.path);
      if (!route) {
        next();
        return;
      }
      try {
        if (!Buffer.isBuffer(request.body)) {
          throw new Error('Express raw body middleware was not installed');
        }
        let call: McpCall | A2APaymentCall | undefined = route.mcpTool === undefined
          ? undefined
          : this.mcpCall(request.body, route.mcpTool);
        if (route.mcpTool !== undefined && !call) {
          next();
          return;
        }
        const headers = this.headers(request);
        const runId = this.textHeader(request.headers['x-run-id']);
        if (!runId) {
          response.status(400).json({
            error: {
              code: 'PAYMENT_CONTEXT_REQUIRED',
              message: 'x-run-id is required',
            },
          });
          return;
        }
        const protocolName = route.protocol ?? 'x402';
        const operation: PayeeOperation = {
          scenarioId: route.scenarioId, runId, priceId: route.priceId,
          resource: route.resource, origin: route.origin, protocol: protocolName,
          requestHash: this.requestHash(request.method, request.path, request.body),
        };
        if (route.a2aTaskStore) {
          call = await this.core.a2a().prepare(request.body, operation, route.a2aTaskStore, headers, response.locals.a2aContext as ServerCallContext | undefined);
          if (!call) { next(); return; }
        }
        const protocol = this.core.protocol(protocolName);
        let credentialPayload: unknown | undefined;
        try {
          const httpCredential = this.core.presentedCredential(protocolName, headers);
          if (call && httpCredential !== undefined) {
            throw new Error('MCP payment credentials must use protocol metadata, not HTTP headers');
          }
          credentialPayload = call instanceof A2APaymentCall ? call.credential
            : call ? this.core.presentedMcpCredential(protocolName, call.meta) : httpCredential;
        } catch (error) {
          throw new InvalidCredentialError(
            error instanceof Error ? error.message : 'invalid payment credential',
          );
        }
        const requestHash = this.requestHash(
          request.method,
          request.path,
          call?.canonicalBody ?? request.body,
        );
        let outcome;
        try {
          outcome = await this.core.protect({ ...operation, requestHash, credentialPayload });
        } catch (error) {
          if (error instanceof InvalidPaymentError) {
            if (call) await this.sendMcpChallenge(response, call, error.challenge, true);
            else this.sendHttpChallenge(response, protocol, error.challenge);
            return;
          }
          throw error;
        }
        if (outcome.outcome === 'payment_required') {
          if (call) await this.sendMcpChallenge(response, call, outcome.challenge, false);
          else this.sendHttpChallenge(response, protocol, outcome.challenge);
          return;
        }
        response.locals.agentvaultPayment = outcome.payment;
        let receiptInstalled: () => boolean;
        if (call) {
          request.body = Buffer.from(call.canonicalBody);
          if (call instanceof A2APaymentCall) receiptInstalled = this.installA2aReceipt(response, call, outcome.challenge, outcome.payment);
          else {
            const receipt = this.core.mcpReceipt(outcome.challenge, outcome.payment);
            receiptInstalled = this.installMcpReceipt(response, call, receipt);
          }
        } else {
          const receipt = protocol.receipt(outcome.payment);
          receiptInstalled = this.installHttpReceipt(response, receipt.headers);
        }
        response.once('finish', () => {
          if (response.statusCode >= 200 && response.statusCode < 300 && receiptInstalled()) {
            console.info(JSON.stringify({
              event: 'agentvault.sdk.payee.resource_released',
              runId,
              intentId: outcome.payment.intent.id,
            }));
          }
        });
        next();
      } catch (error) {
        const status = error instanceof ApiError
          ? error.status
          : error instanceof InvalidCredentialError
            ? 400
            : 502;
        const code = error instanceof ApiError
          ? error.code
          : error instanceof InvalidCredentialError
            ? 'PAYMENT_INVALID'
            : 'AGENTVAULT_UNAVAILABLE';
        response.status(status).json({
          error: {
            code,
            message: error instanceof Error ? error.message : 'payment middleware failed',
          },
        });
      }
    };
  }

  private sendHttpChallenge(
    response: import('express').Response,
    protocol: PaymentProtocol,
    challenge: PaymentChallenge,
  ): void {
    const wire = protocol.challenge(challenge);
    response.status(402).type(wire.contentType ?? 'application/json');
    response.set('payment-protocol', challenge.protocol);
    response.set('payment-expires-at', String(challenge.expires_at));
    for (const [name, value] of Object.entries(wire.headers)) response.set(name, value);
    response.send(wire.body ? Buffer.from(wire.body) : Buffer.alloc(0));
  }

  private async sendMcpChallenge(
    response: import('express').Response,
    call: McpCall | A2APaymentCall,
    challenge: PaymentChallenge,
    rejected: boolean,
  ): Promise<void> {
    const wire = call instanceof A2APaymentCall
      ? await this.core.a2a().challenge(call, challenge, rejected) : this.core.mcpPaymentRequired(challenge, rejected);
    if (call instanceof A2APaymentCall) {
      const extension = this.core.a2a().extension(challenge.protocol);
      if (extension) response.setHeader('x-a2a-extensions', extension);
    }
    response.status(200).type('application/json');
    response.set('payment-protocol', challenge.protocol);
    response.set('payment-expires-at', String(challenge.expires_at));
    response.json({
      jsonrpc: '2.0',
      id: call.id,
      ...wire,
    });
  }

  private installMcpReceipt(
    response: import('express').Response,
    call: McpCall,
    receipt: McpMetadata,
  ): () => boolean {
    const originalEnd = response.end.bind(response);
    const originalWriteHead = response.writeHead.bind(response);
    const chunks: Buffer[] = [];
    let size = 0, submitted = false, committing = false, installed = false;
    let invalid = false;
    const append = (chunk: unknown, encoding: unknown): void => {
      if (chunk === undefined || chunk === null) return;
      if (typeof chunk !== 'string' && !(chunk instanceof Uint8Array)) {
        invalid = true;
        return;
      }
      const codec = typeof encoding === 'string' ? encoding as BufferEncoding : 'utf8';
      size += typeof chunk === 'string' ? Buffer.byteLength(chunk, codec) : chunk.byteLength;
      if (size > this.maxBodyBytes) { invalid = true; chunks.length = 0; }
      else if (!invalid) chunks.push(typeof chunk === 'string' ? Buffer.from(chunk, codec) : Buffer.from(chunk));
    };
    const callback = (args: unknown[]): (() => void) | undefined => {
      const last = args.at(-1);
      return typeof last === 'function' ? last as () => void : undefined;
    };
    response.writeHead = ((...args: Parameters<typeof response.writeHead>) => {
      if (committing) return originalWriteHead(...args);
      if (submitted) throw new Error('paid MCP handler attempted a second response');
      response.statusCode = args[0];
      const raw = args as unknown[];
      if (typeof raw[1] === 'string') response.statusMessage = raw[1];
      const headers = typeof raw[1] === 'object' ? raw[1] : raw[2];
      if (Array.isArray(headers)) {
        for (let index = 0; index + 1 < headers.length; index += 2) {
          response.setHeader(String(headers[index]), headers[index + 1] as string);
        }
      } else if (headers && typeof headers === 'object') {
        for (const [name, value] of Object.entries(headers)) {
          if (value !== undefined) response.setHeader(name, value as string | number | string[]);
        }
      }
      if (String(response.getHeader('content-type') ?? '').toLowerCase().startsWith('text/event-stream')) {
        invalid = true;
        response.end();
      }
      return response;
    }) as typeof response.writeHead;
    response.write = ((...args: unknown[]) => {
      if (submitted) throw new Error('paid MCP handler attempted to write after its response');
      if (String(response.getHeader('content-type') ?? '').toLowerCase().startsWith('text/event-stream')) {
        invalid = true;
      } else append(args[0], args[1]);
      if (invalid) response.end();
      const done = callback(args);
      if (done) queueMicrotask(done);
      return true;
    }) as typeof response.write;
    response.end = ((...args: unknown[]) => {
      if (submitted) return response;
      submitted = true;
      append(typeof args[0] === 'function' ? undefined : args[0], args[1]);
      let bytes: Buffer;
      try {
        const type = String(response.getHeader('content-type') ?? '').split(';')[0]?.trim().toLowerCase();
        if (invalid || type !== 'application/json' || response.hasHeader('content-encoding')) {
          throw new Error('paid MCP response must be bounded, uncompressed JSON');
        }
        bytes = this.mcpResponse(response, call, receipt, Buffer.concat(chunks));
        installed = response.statusCode >= 200 && response.statusCode < 300;
      } catch {
        installed = false;
        response.statusCode = 502;
        response.statusMessage = 'Bad Gateway';
        for (const name of this.core.merchantEvidenceHeaderNames()) response.removeHeader(name);
        bytes = Buffer.from(JSON.stringify({error:{code:'MCP_RESPONSE_INVALID', message:'paid MCP handler returned an invalid response'}}));
      }
      chunks.length = 0;
      response.removeHeader('transfer-encoding');
      response.removeHeader('content-encoding');
      response.removeHeader('etag');
      response.setHeader('content-type', 'application/json');
      response.setHeader('content-length', bytes.length);
      committing = true;
      originalEnd(bytes, callback(args));
      return response;
    }) as typeof response.end;
    response.flushHeaders = () => {
      invalid = true;
      response.end();
    };
    return () => installed;
  }

  private mcpResponse(
    response: import('express').Response,
    call: McpCall,
    receipt: McpMetadata,
    bytes: Buffer,
  ): Buffer {
    const body = JSONRPCResponseSchema.parse(JSON.parse(bytes.toString('utf8')));
    if (body.id !== call.id) {
      throw new Error('paid MCP response must match its JSON-RPC request');
    }
    if (this.hasMerchantEvidenceHeader(this.responseHeaders(response))) {
      throw new Error('paid MCP handler attempted to supply settlement evidence');
    }
    const result = 'result' in body ? body.result : undefined;
    const existing = result?._meta ?? {};
    if (!this.isRecord(existing)) throw new Error('paid MCP result._meta must be an object');
    if (this.core.mcpMerchantEvidence(existing)[1] || receipt.key in existing) {
      throw new Error('paid MCP handler attempted to supply settlement metadata');
    }
    if (response.statusCode < 200 || response.statusCode >= 300) return bytes;
    if (!result) throw new Error('paid MCP handler must return a matching JSON-RPC result');
    const transformed = Buffer.from(JSON.stringify({
      ...body, result:{...result, _meta:{...existing, [receipt.key]:receipt.value}},
    }));
    if (transformed.length > this.maxBodyBytes) throw new Error('paid MCP response exceeds configured limit');
    return transformed;
  }

  private installA2aReceipt(response: import('express').Response, call: A2APaymentCall,
    challenge: PaymentChallenge, payment: SettledIncomingPayment): () => boolean {
    const originalJson = response.json.bind(response), originalSend = response.send.bind(response);
    let released = false, committing = false, submitted = false;
    response.write = (() => { throw new Error('paid A2A responses cannot stream'); }) as typeof response.write;
    response.flushHeaders = () => { throw new Error('paid A2A responses cannot stream'); };
    response.send = ((body: unknown) => {
      if (!committing) throw new Error('paid A2A handler must return JSON-RPC via response.json');
      return originalSend(body);
    }) as typeof response.send;
    response.json = ((body: unknown) => {
      if (submitted) throw new Error('A2A handler attempted a second response');
      submitted = true;
      void (async () => {
        if (this.hasMerchantEvidenceHeader(this.responseHeaders(response))) throw new Error('handler supplied payment headers');
        if (Buffer.byteLength(JSON.stringify(body)) > this.maxBodyBytes) throw new Error('A2A response exceeds limit');
        const paid = await this.core.a2a().receipt(call, body, challenge, payment);
        if (Buffer.byteLength(JSON.stringify(paid)) > this.maxBodyBytes) throw new Error('A2A receipt exceeds limit');
        response.removeHeader('content-length');
        committing = true;
        originalJson(paid);
        released = true;
      })().catch((error: unknown) => {
        committing = true;
        response.status(502);
        originalJson({error: {code: 'A2A_RESPONSE_INVALID', message: error instanceof Error ? error.message : 'invalid A2A response'}});
      });
      return response;
    }) as typeof response.json;
    return () => released;
  }

  private installHttpReceipt(
    response: import('express').Response,
    receiptHeaders: Readonly<Record<string, string>>,
  ): () => boolean {
    const originalWriteHead = response.writeHead.bind(response);
    const originalSetHeader = response.setHeader.bind(response);
    let installed = false;
    response.writeHead = ((...args: Parameters<typeof response.writeHead>) => {
      if (!installed) {
        const candidates = this.responseHeaders(response);
        const rawArgs = args as unknown[];
        const explicit = typeof rawArgs[1] === 'object' ? rawArgs[1]
          : typeof rawArgs[2] === 'object' ? rawArgs[2] : undefined;
        if (Array.isArray(explicit)) {
          for (let index = 0; index + 1 < explicit.length; index += 2) {
            const name = explicit[index];
            const value = explicit[index + 1];
            if (typeof name === 'string' && value !== undefined) {
              candidates.set(name, String(value));
            }
          }
        } else if (explicit && typeof explicit === 'object') {
          for (const [name, value] of Object.entries(explicit)) {
            if (Array.isArray(value)) candidates.set(name, value.join(', '));
            else if (value !== undefined) candidates.set(name, String(value));
          }
        }
        if (this.hasMerchantEvidenceHeader(candidates)) {
          throw new Error('paid handler attempted to supply settlement evidence');
        }
        for (const [name, value] of Object.entries(receiptHeaders)) {
          originalSetHeader(name, value);
        }
        installed = true;
      }
      return originalWriteHead(...args);
    }) as typeof response.writeHead;
    return () => installed;
  }

  private hasMerchantEvidenceHeader(headers: Headers): boolean {
    return this.core.merchantEvidenceHeaderNames().some((name) => headers.has(name));
  }

  private responseHeaders(response: import('express').Response): Headers {
    const headers = new Headers();
    for (const [name, value] of Object.entries(response.getHeaders())) {
      if (Array.isArray(value)) headers.set(name, value.join(', '));
      else if (value !== undefined) headers.set(name, String(value));
    }
    return headers;
  }

  private mcpCall(body: Buffer, expected: string): McpCall | undefined {
    let value: unknown;
    try {
      value = JSON.parse(body.toString('utf8')) as unknown;
    } catch {
      return undefined;
    }
    if (!this.isRecord(value) || value.jsonrpc !== '2.0' || value.method !== 'tools/call' ||
        !this.isRecord(value.params) || value.params.name !== expected) return undefined;
    if (typeof value.id !== 'string' && typeof value.id !== 'number') {
      throw new InvalidCredentialError(
        'paid MCP tools/call requires a string or numeric JSON-RPC id',
      );
    }
    const nestedMeta = value.params._meta ?? {};
    if (!this.isRecord(nestedMeta)) {
      throw new InvalidCredentialError('MCP params._meta must be an object');
    }
    const rootMeta = value._meta ?? {};
    if (!this.isRecord(rootMeta)) {
      throw new InvalidCredentialError('JSON-RPC root _meta must be an object');
    }
    const meta: Record<string, unknown> = { ...nestedMeta };
    for (const key of this.core.mcpRootRequestMetadataKeys()) {
      if (key in rootMeta) {
        if (key in meta) {
          throw new InvalidCredentialError('duplicate MCP payment credential metadata');
        }
        meta[key] = rootMeta[key];
      }
    }
    const canonical = structuredClone(value);
    if (!this.isRecord(canonical.params)) {
      throw new InvalidCredentialError('MCP params must be an object');
    }
    const canonicalMeta = this.isRecord(canonical.params._meta)
      ? canonical.params._meta
      : {};
    for (const key of this.core.mcpRequestMetadataKeys()) delete canonicalMeta[key];
    if (Object.keys(canonicalMeta).length > 0) canonical.params._meta = canonicalMeta;
    else delete canonical.params._meta;
    if (this.isRecord(canonical._meta)) {
      for (const key of this.core.mcpRootRequestMetadataKeys()) delete canonical._meta[key];
      if (Object.keys(canonical._meta).length === 0) delete canonical._meta;
    }
    return {
      id: value.id,
      meta,
      canonicalBody: Buffer.from(this.canonicalJson(canonical)),
    };
  }

  private headers(request: import('express').Request): Headers {
    const paymentHeaders = new Set(
      this.core.credentialHeaderNames().map((name) => name.toLowerCase()),
    );
    const counts = new Map<string, number>();
    for (let index = 0; index < request.rawHeaders.length; index += 2) {
      const name = request.rawHeaders[index]?.toLowerCase();
      if (name && paymentHeaders.has(name)) counts.set(name, (counts.get(name) ?? 0) + 1);
    }
    const duplicate = [...counts.entries()].find(([, count]) => count > 1)?.[0];
    if (duplicate) throw new InvalidCredentialError(
      `duplicate payment request header is not allowed: ${duplicate}`,
    );
    const headers = new Headers();
    for (const [name, value] of Object.entries(request.headers)) {
      if (typeof value === 'string') headers.set(name, value);
      else if (Array.isArray(value)) headers.set(name, value.join(', '));
    }
    return headers;
  }

  private requestHash(method: string, path: string, body: Buffer): string {
    return createHash('sha256')
      .update('agentvault.payee.http.v1\0')
      .update(method.toUpperCase())
      .update('\0')
      .update(path)
      .update('\0')
      .update(body)
      .digest('hex');
  }

  private canonicalJson(value: unknown): string {
    if (Array.isArray(value)) return `[${value.map((item) => this.canonicalJson(item)).join(',')}]`;
    if (this.isRecord(value)) {
      return `{${Object.keys(value).sort().map((key) =>
        `${JSON.stringify(key)}:${this.canonicalJson(value[key])}`).join(',')}}`;
    }
    const primitive = JSON.stringify(value);
    if (primitive === undefined) throw new Error('MCP message contains a non-JSON value');
    return primitive;
  }

  private textHeader(value: string | string[] | undefined): string | undefined {
    const result = Array.isArray(value) ? value[0] : value;
    return result?.trim() ? result : undefined;
  }

  private isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
  }
}

class InvalidCredentialError extends Error {}
