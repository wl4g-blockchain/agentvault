import type { Task } from '@a2a-js/sdk';
import type { HttpCredential, PaymentChallenge, PaymentCredential } from '../model/base.js';
import type { SettledIncomingPayment } from '../model/payee.js';
import { MppAdapter } from './mpp.js';
import { X402Adapter } from './x402.js';

export interface ProtocolHttpResponse {
  headers: Readonly<Record<string, string>>;
  contentType?: string;
  body?: Uint8Array;
}

export interface MerchantEvidence {
  headerName: string;
  headerValue: string;
}

export interface McpMetadata {
  key: string;
  value: unknown;
}

/** Exactly one of result or error is set by a protocol provider. */
export type McpPaymentRequired =
  | { result: unknown; error?: never }
  | { result?: never; error: unknown };

export interface PaymentProtocol {
  readonly name: string;
  credentialHeaderNames(): readonly string[];
  merchantEvidenceHeaderNames?(): readonly string[];
  credential(credential: PaymentCredential): HttpCredential;
  merchantEvidence(headers: Headers): MerchantEvidence | undefined;
  presentedCredential(headers: Headers, limit: number): unknown | undefined;
  challenge(challenge: PaymentChallenge): ProtocolHttpResponse;
  receipt(payment: SettledIncomingPayment): ProtocolHttpResponse;
}

/** Optional message-level SPI implemented by MCP-capable protocols. */
export interface McpPaymentProtocol extends PaymentProtocol {
  mcpRequestMetadataKey(): string;
  mcpRootRequestMetadataKey?(): string;
  mcpReceiptMetadataKey(): string;
  mcpChallenge(message: unknown, limit: number): ProtocolHttpResponse | undefined;
  mcpCredential(credential: PaymentCredential): McpMetadata;
  presentedMcpCredential(meta: unknown, limit: number): unknown | undefined;
  mcpPaymentRequired(challenge: PaymentChallenge): McpPaymentRequired;
  mcpPaymentRejected(challenge: PaymentChallenge, message: string): McpPaymentRequired;
  mcpReceipt(challenge: PaymentChallenge, payment: SettledIncomingPayment): McpMetadata;
  mcpMerchantEvidence(meta: unknown, limit: number): MerchantEvidence | undefined;
}

export interface A2APaymentProtocol extends PaymentProtocol {
  a2aExtension(): string;
  a2aChallenge(message: Record<string, unknown>, limit: number): ProtocolHttpResponse | undefined;
  a2aApplyCredential(message: Record<string, unknown>, credential: PaymentCredential): void;
  presentedA2aCredential(message: Record<string, unknown>, limit: number): unknown | undefined;
  a2aStripCredential(message: Record<string, unknown>): void;
  a2aPaymentRequired(challenge: PaymentChallenge, task: Task, rejected: boolean): McpPaymentRequired;
  a2aEvidence(message: Record<string, unknown>, limit: number): MerchantEvidence | undefined;
  a2aInjectReceipt(message: Record<string, unknown>, challenge: PaymentChallenge, payment: SettledIncomingPayment): void;
}

export type PaymentProtocolProvider = () => PaymentProtocol;

export class ProtocolRegistry {
  private readonly priority: readonly string[];

  constructor(private readonly adapters: ReadonlyMap<string, PaymentProtocol>) {
    this.priority = [...adapters.keys()];
  }

  get(name: string): PaymentProtocol {
    const adapter = this.adapters.get(name);
    if (!adapter) throw new Error(`payment protocol is not enabled: ${name}`);
    return adapter;
  }

  mcp(name: string): McpPaymentProtocol {
    const adapter = this.get(name);
    if (!isMcpPaymentProtocol(adapter)) {
      throw new Error(`payment protocol does not support MCP: ${name}`);
    }
    return adapter;
  }

  a2a(name: string): A2APaymentProtocol {
    const adapter = this.get(name) as Partial<A2APaymentProtocol>;
    if (typeof adapter.a2aChallenge !== 'function') throw new Error(`protocol does not support A2A: ${name}`);
    return adapter as A2APaymentProtocol;
  }

  a2aChallenge(message: Record<string, unknown>, limit: number): readonly [A2APaymentProtocol, ProtocolHttpResponse] | undefined {
    for (const name of this.priority) {
      const adapter = this.a2a(name), value = adapter.a2aChallenge(message, limit);
      if (value) return [adapter, value];
    }
    return undefined;
  }

  a2aCredential(message: Record<string, unknown>, limit: number): readonly [string | undefined, unknown] {
    let name: string | undefined, value: unknown;
    for (const key of this.priority) {
      const found = this.a2a(key).presentedA2aCredential(message, limit);
      if (found !== undefined) {
        if (name) throw new Error('multiple A2A payment credentials');
        name = key; value = found;
      }
    }
    return [name, value];
  }

  a2aStripCredentials(message: Record<string, unknown>): void {
    for (const name of this.priority) this.a2a(name).a2aStripCredential(message);
  }

  a2aEvidence(message: Record<string, unknown>, limit: number): readonly [string | undefined, MerchantEvidence | undefined] {
    let name: string | undefined, evidence: MerchantEvidence | undefined;
    for (const key of this.priority) {
      const found = this.a2a(key).a2aEvidence(message, limit);
      if (found) {
        if (name) throw new Error('multiple A2A payment receipts');
        name = key; evidence = found;
      }
    }
    return [name, evidence];
  }

  a2aExtensions(existing = ''): string {
    const values = new Set(existing.split(',').map((value) => value.trim()).filter(Boolean));
    for (const key of this.priority) {
      const extension = this.a2a(key).a2aExtension();
      if (extension) values.add(extension);
    }
    return [...values].join(', ');
  }

  credentialHeaderNames(): readonly string[] {
    const names: string[] = [];
    for (const name of this.priority) {
      for (const candidate of this.get(name).credentialHeaderNames()) {
        if (!names.some((existing) => existing.toLowerCase() === candidate.toLowerCase())) {
          names.push(candidate);
        }
      }
    }
    return names;
  }

  merchantEvidenceHeaderNames(): readonly string[] {
    const names: string[] = [];
    for (const name of this.priority) {
      for (const candidate of this.get(name).merchantEvidenceHeaderNames?.() ?? []) {
        if (!names.some((existing) => existing.toLowerCase() === candidate.toLowerCase())) {
          names.push(candidate);
        }
      }
    }
    return names;
  }

  presentedCredential(
    headers: Headers,
    limit: number,
  ): readonly [string | undefined, unknown | undefined] {
    let selected: string | undefined;
    let payload: unknown | undefined;
    for (const name of this.priority) {
      const value = this.get(name).presentedCredential(headers, limit);
      if (value === undefined) continue;
      if (selected !== undefined) throw new Error('multiple payment protocol credentials were supplied');
      selected = name;
      payload = value;
    }
    return [selected, payload];
  }

  merchantEvidence(
    headers: Headers,
  ): readonly [string | undefined, MerchantEvidence | undefined] {
    let selected: string | undefined;
    let evidence: MerchantEvidence | undefined;
    for (const name of this.priority) {
      const value = this.get(name).merchantEvidence(headers);
      if (!value) continue;
      if (selected !== undefined) throw new Error('multiple payment protocol receipts were supplied');
      selected = name;
      evidence = value;
    }
    return [selected, evidence];
  }

  mcpChallenge(
    message: unknown,
    limit: number,
  ): { adapter: McpPaymentProtocol; challenge: ProtocolHttpResponse } | undefined {
    for (const name of this.priority) {
      const adapter = this.mcp(name);
      const challenge = adapter.mcpChallenge(message, limit);
      if (challenge) return { adapter, challenge };
    }
    return undefined;
  }

  presentedMcpCredential(
    meta: unknown,
    limit: number,
  ): readonly [string | undefined, unknown | undefined] {
    let selected: string | undefined;
    let payload: unknown | undefined;
    for (const name of this.priority) {
      const value = this.mcp(name).presentedMcpCredential(meta, limit);
      if (value === undefined) continue;
      if (selected !== undefined) throw new Error('multiple MCP payment credentials were supplied');
      selected = name;
      payload = value;
    }
    return [selected, payload];
  }

  mcpMerchantEvidence(
    meta: unknown,
    limit: number,
  ): readonly [string | undefined, MerchantEvidence | undefined] {
    let selected: string | undefined;
    let evidence: MerchantEvidence | undefined;
    for (const name of this.priority) {
      const value = this.mcp(name).mcpMerchantEvidence(meta, limit);
      if (!value) continue;
      if (selected !== undefined) throw new Error('multiple MCP settlement receipts were supplied');
      selected = name;
      evidence = value;
    }
    return [selected, evidence];
  }

  mcpRequestMetadataKeys(): readonly string[] {
    return this.priority.map((name) => this.mcp(name).mcpRequestMetadataKey());
  }

  mcpRootRequestMetadataKeys(): readonly string[] {
    return this.priority.flatMap((name) => {
      const key = this.mcp(name).mcpRootRequestMetadataKey?.();
      return key === undefined ? [] : [key];
    });
  }

  mcpReceiptMetadataKeys(): readonly string[] {
    return this.priority.map((name) => this.mcp(name).mcpReceiptMetadataKey());
  }
}

function isMcpPaymentProtocol(adapter: PaymentProtocol): adapter is McpPaymentProtocol {
  return typeof (adapter as Partial<McpPaymentProtocol>).mcpChallenge === 'function' &&
    typeof (adapter as Partial<McpPaymentProtocol>).mcpCredential === 'function';
}

/** Creates isolated protocol providers; failures are never cached. */
export class PaymentProtocolFactory {
  private readonly providers = new Map<string, PaymentProtocolProvider>();

  constructor() {
    this.providers.set('x402', () => new X402Adapter());
    this.providers.set('mpp', () => new MppAdapter());
  }

  register(name: string, provider: PaymentProtocolProvider): void {
    if (!name || name !== name.toLowerCase() || this.providers.has(name)) {
      throw new Error(`invalid or duplicate payment protocol provider: ${name}`);
    }
    this.providers.set(name, provider);
  }

  create(enabled: readonly string[]): ProtocolRegistry {
    if (enabled.length === 0) throw new Error('payment protocol configuration is empty');
    const adapters = new Map<string, PaymentProtocol>();
    for (const name of enabled) {
      const provider = this.providers.get(name);
      if (!provider) throw new Error(`payment protocol provider is unavailable: ${name}`);
      if (adapters.has(name)) throw new Error(`duplicate payment protocol provider: ${name}`);
      const adapter = provider();
      if (adapter.name !== name) throw new Error(`payment protocol provider identity mismatch: ${name}`);
      adapters.set(name, adapter);
    }
    return new ProtocolRegistry(adapters);
  }
}
