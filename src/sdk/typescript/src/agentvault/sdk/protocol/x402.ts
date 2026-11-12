import { randomUUID } from 'node:crypto';
import type { Task, Message } from '@a2a-js/sdk';
import { A2ACall } from '../model/a2a.js';
import {
  decodePaymentResponseHeader,
  decodePaymentSignatureHeader,
  encodePaymentRequiredHeader,
  encodePaymentResponseHeader,
  encodePaymentSignatureHeader,
} from '@x402/core/http';
import { parsePaymentPayload, parsePaymentRequired } from '@x402/core/schemas';
import type { PaymentPayload, PaymentRequired, SettleResponse } from '@x402/core/types';
import {
  extractPaymentFromMeta,
  extractPaymentResponseFromMeta,
  MCP_PAYMENT_META_KEY,
  MCP_PAYMENT_RESPONSE_META_KEY,
} from '@x402/mcp';
import { isDeepStrictEqual } from 'node:util';

import type { HttpCredential, PaymentChallenge, PaymentCredential } from '../model/base.js';
import type { SettledIncomingPayment } from '../model/payee.js';
import type {
  McpMetadata,
  McpPaymentProtocol,
  McpPaymentRequired,
  MerchantEvidence,
  ProtocolHttpResponse,
} from './adapter.js';

type Eip3009PaymentPayload = PaymentPayload & {
  accepted: PaymentPayload['accepted'] & { network: `eip155:${string}` };
  payload: {
    signature: string;
    authorization: {
      from: string;
      to: string;
      value: string;
      validAfter: string;
      validBefore: string;
      nonce: string;
    };
  };
};

type SolanaExactPaymentPayload = PaymentPayload & {
  accepted: PaymentPayload['accepted'] & { network: `solana:${string}` };
  payload: { transaction: string };
};

type ExactPaymentPayload = Eip3009PaymentPayload | SolanaExactPaymentPayload;

/** x402 v2 wire adapter. Signing and settlement authority stay in AgentVault. */
export class X402Adapter implements McpPaymentProtocol {
  readonly name = 'x402';

  credentialHeaderNames(): readonly string[] {
    return ['payment-signature'];
  }

  merchantEvidenceHeaderNames(): readonly string[] {
    return ['payment-response'];
  }

  credential(credential: PaymentCredential): HttpCredential {
    const delivery = credential.http;
    if (credential.protocol !== this.name || !delivery ||
        delivery.header_name.toLowerCase() !== 'payment-signature') {
      throw new Error('x402 credential has no PAYMENT-SIGNATURE delivery');
    }
    this.paymentPayload(
      decodePaymentSignatureHeader(delivery.header_value),
      'PAYMENT-SIGNATURE',
    );
    return delivery;
  }

  merchantEvidence(headers: Headers): MerchantEvidence | undefined {
    const value = headers.get('payment-response');
    if (!value) return undefined;
    this.settlement(decodePaymentResponseHeader(value), 'PAYMENT-RESPONSE');
    return { headerName: 'payment-response', headerValue: value };
  }

  presentedCredential(headers: Headers, limit: number): unknown | undefined {
    const value = headers.get('payment-signature');
    if (!value) return undefined;
    this.boundedHeader(value, limit, 'PAYMENT-SIGNATURE');
    return this.paymentPayload(
      decodePaymentSignatureHeader(value),
      'PAYMENT-SIGNATURE',
    );
  }

  challenge(challenge: PaymentChallenge): ProtocolHttpResponse {
    const required = this.storedPaymentRequired(challenge);
    const body = Buffer.from(JSON.stringify(required));
    return {
      headers: { 'payment-required': encodePaymentRequiredHeader(required) },
      contentType: 'application/json',
      body,
    };
  }

  receipt(payment: SettledIncomingPayment): ProtocolHttpResponse {
    return {
      headers: {
        'payment-response': encodePaymentResponseHeader(this.paymentSettlement(payment)),
      },
    };
  }

  mcpRequestMetadataKey(): string {
    return MCP_PAYMENT_META_KEY;
  }

  mcpReceiptMetadataKey(): string {
    return MCP_PAYMENT_RESPONSE_META_KEY;
  }

  mcpChallenge(message: unknown, limit: number): ProtocolHttpResponse | undefined {
    this.boundedJson(message, limit, 'MCP x402 message');
    if (!this.isRecord(message) || !this.isRecord(message.result) ||
        message.result.isError !== true) return undefined;

    const result = message.result;
    let structured: PaymentRequired | undefined;
    if (this.isRecord(result.structuredContent) &&
        ('x402Version' in result.structuredContent || 'accepts' in result.structuredContent)) {
      structured = this.paymentRequired(result.structuredContent, 'MCP x402 structuredContent');
    }
    let text: PaymentRequired | undefined;
    if (Array.isArray(result.content)) {
      for (const item of result.content) {
        if (!this.isRecord(item) || item.type !== 'text' || typeof item.text !== 'string') continue;
        try {
          text = this.paymentRequired(JSON.parse(item.text) as unknown, 'MCP x402 content text');
          break;
        } catch {
          // Non-payment text content is allowed alongside the payment challenge.
        }
      }
    }
    if (!structured && !text) return undefined;
    if (structured && text && !isDeepStrictEqual(structured, text)) {
      throw new Error('MCP x402 structuredContent conflicts with content text');
    }
    const required = structured ?? text!;
    const body = Buffer.from(JSON.stringify(required));
    if (body.length > limit) throw new Error('MCP x402 PaymentRequired exceeds configured limit');
    return {
      headers: { 'payment-required': encodePaymentRequiredHeader(required) },
      contentType: 'application/json',
      body,
    };
  }

  mcpCredential(credential: PaymentCredential): McpMetadata {
    const delivery = this.credential(credential);
    return {
      key: MCP_PAYMENT_META_KEY,
      value: this.paymentPayload(
        decodePaymentSignatureHeader(delivery.header_value),
        'PAYMENT-SIGNATURE',
      ),
    };
  }

  presentedMcpCredential(meta: unknown, limit: number): unknown | undefined {
    if (!this.isRecord(meta)) throw new Error('MCP params._meta must be an object');
    if (!(MCP_PAYMENT_META_KEY in meta)) return undefined;
    this.boundedJson(meta[MCP_PAYMENT_META_KEY], limit, 'MCP x402 payment metadata');
    const payment = extractPaymentFromMeta({
      name: 'agentvault',
      _meta: meta,
    });
    if (!payment) throw new Error('MCP x402 payment metadata is not PaymentPayload v2');
    return this.paymentPayload(payment, 'MCP x402 payment metadata');
  }

  mcpPaymentRequired(challenge: PaymentChallenge): McpPaymentRequired {
    return { result: this.paymentResult(this.storedPaymentRequired(challenge)) };
  }

  mcpPaymentRejected(challenge: PaymentChallenge, message: string): McpPaymentRequired {
    return {
      result: this.paymentResult({
        ...this.storedPaymentRequired(challenge),
        error: message,
      }),
    };
  }

  mcpReceipt(_challenge: PaymentChallenge, payment: SettledIncomingPayment): McpMetadata {
    return {
      key: MCP_PAYMENT_RESPONSE_META_KEY,
      value: this.paymentSettlement(payment),
    };
  }

  mcpMerchantEvidence(meta: unknown, limit: number): MerchantEvidence | undefined {
    if (!this.isRecord(meta)) throw new Error('MCP result._meta must be an object');
    if (!(MCP_PAYMENT_RESPONSE_META_KEY in meta)) return undefined;
    this.boundedJson(meta[MCP_PAYMENT_RESPONSE_META_KEY], limit, 'MCP x402 receipt metadata');
    const settlement = extractPaymentResponseFromMeta({ _meta: meta });
    if (!settlement) throw new Error('MCP x402 settlement metadata is invalid');
    const confirmed = this.settlement(settlement, 'MCP x402 settlement metadata');
    return {
      headerName: 'payment-response',
      headerValue: encodePaymentResponseHeader(confirmed),
    };
  }

  a2aExtension(): string { return 'https://github.com/google-agentic-commerce/a2a-x402/blob/main/spec/v0.2'; }

  a2aChallenge(message: Record<string, unknown>, limit: number): ProtocolHttpResponse | undefined {
    const result = A2ACall.optionalObject(message.result);
    if (!result) return undefined;
    const meta = this.a2aStatusMetadata(result);
    if (!('x402.payment.required' in meta)) {
      if (meta['x402.payment.status'] === 'payment-required') throw new Error('embedded A2A payment mandates are unsupported');
      return undefined;
    }
    const task = A2ACall.task(message);
    if (task.status.state !== 'input-required' || meta['x402.payment.status'] !== 'payment-required') throw new Error('invalid A2A challenge state');
    return this.mcpChallenge({result: {isError: true, structuredContent: meta['x402.payment.required']}}, limit);
  }

  a2aApplyCredential(message: Record<string, unknown>, credential: PaymentCredential): void {
    const meta = A2ACall.metadata(message);
    if (Object.keys(meta).some((key) => key.startsWith('x402.payment.'))) throw new Error('A2A request already contains payment metadata');
    meta['x402.payment.status'] = 'payment-submitted';
    meta['x402.payment.payload'] = this.mcpCredential(credential).value;
  }

  presentedA2aCredential(message: Record<string, unknown>, limit: number): unknown | undefined {
    const meta = A2ACall.metadata(message), keys = Object.keys(meta).filter((key) => key.startsWith('x402.payment.'));
    if (!keys.length) return undefined;
    if (keys.length !== 2 || !keys.includes('x402.payment.payload') || meta['x402.payment.status'] !== 'payment-submitted') throw new Error('invalid A2A payment credential metadata');
    return this.presentedMcpCredential({[this.mcpRequestMetadataKey()]: meta['x402.payment.payload']}, limit);
  }

  a2aStripCredential(message: Record<string, unknown>): void {
    const meta = A2ACall.metadata(message);
    delete meta['x402.payment.status']; delete meta['x402.payment.payload'];
  }

  a2aPaymentRequired(challenge: PaymentChallenge, task: Task, rejected: boolean): McpPaymentRequired {
    const meta: Record<string, unknown> = {'x402.payment.status': 'payment-required', 'x402.payment.required': this.storedPaymentRequired(challenge)};
    if (rejected) meta['x402.payment.error'] = 'PAYMENT_INVALID';
    const result = structuredClone(task);
    result.status = {state: 'input-required', message: this.a2aMessage(task, 'Payment required', meta)};
    return {result};
  }

  a2aEvidence(message: Record<string, unknown>, limit: number): MerchantEvidence | undefined {
    const result = A2ACall.optionalObject(message.result);
    if (!result) return undefined;
    const meta = this.a2aStatusMetadata(result);
    if (!('x402.payment.receipts' in meta)) return undefined;
    A2ACall.task(message);
    const receipts = meta['x402.payment.receipts'];
    if (!Array.isArray(receipts) || receipts.length !== 1 || meta['x402.payment.status'] !== 'payment-completed') throw new Error('A2A requires exactly one completed receipt');
    return this.mcpMerchantEvidence({[this.mcpReceiptMetadataKey()]: receipts[0]}, limit);
  }

  a2aInjectReceipt(message: Record<string, unknown>, challenge: PaymentChallenge, payment: SettledIncomingPayment): void {
    const task = A2ACall.task(message);
    const statusMessage = task.status.message ?? this.a2aMessage(task, 'Payment completed', {});
    const meta = {...statusMessage.metadata};
    if (Object.keys(meta).some((key) => key.startsWith('x402.payment.'))) throw new Error('handler attempted to supply payment metadata');
    meta['x402.payment.status'] = 'payment-completed'; meta['x402.payment.receipts'] = [this.mcpReceipt(challenge, payment).value];
    task.status.message = {...statusMessage, metadata: meta};
  }

  private a2aMessage(task: Task, text: string, metadata: Record<string, unknown>): Message {
    return {kind: 'message', messageId: randomUUID(), role: 'agent', taskId: task.id, contextId: task.contextId, parts: [{kind:'text', text}], metadata};
  }

  private a2aStatusMetadata(result: Record<string, unknown>): Record<string, unknown> {
    const status = A2ACall.object(result.status);
    const message = A2ACall.optionalObject(status.message);
    return message ? A2ACall.object(message.metadata ?? {}) : {};
  }

  private storedPaymentRequired(challenge: PaymentChallenge): PaymentRequired {
    if (challenge.protocol !== this.name || !this.isRecord(challenge.payload) ||
        challenge.payload.payment_required === undefined) {
      throw new Error('x402 challenge protocol or payload is invalid');
    }
    return this.paymentRequired(challenge.payload.payment_required, 'x402 challenge');
  }

  private paymentRequired(value: unknown, label: string): PaymentRequired {
    const parsed = parsePaymentRequired(value);
    if (!parsed.success || parsed.data.x402Version !== 2 || parsed.data.accepts.length === 0) {
      throw new Error(`${label} is not an official x402 v2 PaymentRequired`);
    }
    return parsed.data as PaymentRequired;
  }

  private paymentPayload(value: unknown, label: string): PaymentPayload {
    const parsed = parsePaymentPayload(value);
    if (!parsed.success || parsed.data.x402Version !== 2) {
      throw new Error(`${label} is not an official x402 v2 PaymentPayload`);
    }
    const payload = parsed.data as PaymentPayload;
    this.validateExactPayload(payload, label);
    return payload;
  }

  private validateExactPayload(
    payload: PaymentPayload,
    label: string,
  ): asserts payload is ExactPaymentPayload {
    if (payload.accepted.scheme !== 'exact') {
      throw new Error(`${label} uses an unsupported x402 scheme`);
    }
    if (payload.accepted.network.startsWith('eip155:')) {
      const authorization = this.isRecord(payload.payload.authorization)
        ? payload.payload.authorization
        : undefined;
      if (!this.hasOnlyKeys(payload.payload, ['signature', 'authorization']) ||
          typeof payload.payload.signature !== 'string' || !payload.payload.signature ||
          !authorization || !this.hasOnlyKeys(authorization, [
            'from', 'to', 'value', 'validAfter', 'validBefore', 'nonce',
          ]) || !['from', 'to', 'value', 'validAfter', 'validBefore', 'nonce']
            .every((name) => typeof authorization[name] === 'string' && authorization[name] !== '')) {
        throw new Error(`${label} is not an official EIP-3009 payload`);
      }
      const extra = payload.accepted.extra;
      if (typeof extra.name !== 'string' || !extra.name ||
          typeof extra.version !== 'string' || !extra.version) {
        throw new Error(`${label} has no EIP-3009 domain metadata`);
      }
      return;
    }
    if (payload.accepted.network.startsWith('solana:')) {
      const transaction = payload.payload.transaction;
      if (!this.hasOnlyKeys(payload.payload, ['transaction']) ||
          typeof transaction !== 'string' || !transaction) {
        throw new Error(`${label} is not an official Solana exact payload`);
      }
      const decoded = Buffer.from(transaction, 'base64');
      if (decoded.length === 0 || decoded.toString('base64') !== transaction) {
        throw new Error(`${label} has no canonical base64 Solana transaction`);
      }
      if (typeof payload.accepted.extra.feePayer !== 'string' ||
          !payload.accepted.extra.feePayer) {
        throw new Error(`${label} has no Solana facilitator fee payer`);
      }
      return;
    }
    throw new Error(`${label} uses an unsupported x402 network`);
  }

  private paymentSettlement(payment: SettledIncomingPayment): SettleResponse {
    const receipt = payment.receipt;
    if (receipt.protocol !== this.name || !receipt.tx_hash || !receipt.payer ||
        !receipt.asset.network) {
      throw new Error('confirmed x402 receipt omitted transaction, payer, or network');
    }
    return {
      success: true,
      payer: receipt.payer,
      transaction: receipt.tx_hash,
      network: receipt.asset.network,
    } as SettleResponse;
  }

  private settlement(value: SettleResponse, label: string): SettleResponse {
    if (!value.success || !value.payer || !value.transaction || !value.network) {
      throw new Error(`${label} is not a successful x402 settlement`);
    }
    return value;
  }

  private paymentResult(required: PaymentRequired): Record<string, unknown> {
    return {
      content: [{ type: 'text', text: JSON.stringify(required) }],
      structuredContent: required,
      isError: true,
    };
  }

  private boundedHeader(value: string, limit: number, label: string): void {
    if (limit <= 0 || value.length > Math.ceil(limit * 4 / 3) + 4 || /[\r\n]/.test(value)) {
      throw new Error(`${label} exceeds configured limit`);
    }
  }

  private boundedJson(value: unknown, limit: number, label: string): void {
    let encoded: string;
    try {
      encoded = JSON.stringify(value);
    } catch {
      throw new Error(`${label} is invalid`);
    }
    if (!encoded || Buffer.byteLength(encoded) > limit) {
      throw new Error(`${label} is empty or oversized`);
    }
  }

  private isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
  }

  private hasOnlyKeys(value: Record<string, unknown>, expected: readonly string[]): boolean {
    const keys = Object.keys(value);
    return keys.length === expected.length && keys.every((key) => expected.includes(key));
  }
}
