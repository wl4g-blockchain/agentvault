import type { Task } from '@a2a-js/sdk';
import { A2ACall } from '../model/a2a.js';
import { Challenge, Credential, Errors, Mcp, Receipt } from 'mppx';
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

/** MPP Tempo charge wire adapter backed by the official mppx models. */
export class MppAdapter implements McpPaymentProtocol {
  readonly name = 'mpp';

  credentialHeaderNames(): readonly string[] {
    return ['payment-authorization', 'authorization'];
  }

  merchantEvidenceHeaderNames(): readonly string[] {
    return ['payment-receipt'];
  }

  credential(credential: PaymentCredential): HttpCredential {
    const delivery = credential.http;
    const header = delivery?.header_name.toLowerCase();
    if (credential.protocol !== this.name || !delivery ||
        (header !== 'authorization' && header !== 'payment-authorization')) {
      throw new Error('MPP credential has no allowed Payment authorization delivery');
    }
    const parsed = this.parseCredential(delivery.header_value, 'MPP credential');
    this.requireTempoCharge(parsed.challenge);
    const expectedHeader = parsed.challenge.header ?? 'Authorization';
    if (expectedHeader.toLowerCase() !== header) {
      throw new Error('MPP credential delivery does not match its challenged header');
    }
    return delivery;
  }

  merchantEvidence(headers: Headers): MerchantEvidence | undefined {
    const value = headers.get('payment-receipt');
    if (!value) return undefined;
    const receipt = this.parseReceipt(value, 'Payment-Receipt');
    this.requireTempoReceipt(receipt);
    return { headerName: 'payment-receipt', headerValue: value };
  }

  presentedCredential(headers: Headers, limit: number): unknown | undefined {
    const dedicated = headers.get('payment-authorization') ?? '';
    const authorization = Credential.extractPaymentScheme(headers.get('authorization') ?? '') ?? '';
    if (dedicated && authorization) throw new Error('multiple MPP payment credentials were supplied');
    const value = dedicated || authorization;
    if (!value) return undefined;
    if (value.length > limit || /[\r\n]/.test(value)) {
      throw new Error('MPP Payment authorization is invalid or oversized');
    }
    const parsed = this.parseCredential(value, 'MPP Payment authorization');
    this.requireTempoCharge(parsed.challenge);
    const actualHeader = dedicated ? 'Payment-Authorization' : 'Authorization';
    const expectedHeader = parsed.challenge.header ?? 'Authorization';
    if (expectedHeader.toLowerCase() !== actualHeader.toLowerCase()) {
      throw new Error('MPP credential delivery does not match its challenged header');
    }
    return { authorization: Credential.serialize(parsed) };
  }

  challenge(challenge: PaymentChallenge): ProtocolHttpResponse {
    if (challenge.protocol !== this.name || !this.isRecord(challenge.payload) ||
        typeof challenge.payload.www_authenticate !== 'string' ||
        /[\r\n]/.test(challenge.payload.www_authenticate)) {
      throw new Error('MPP challenge has no valid WWW-Authenticate value');
    }
    let parsed: Challenge.Challenge;
    try {
      parsed = Challenge.deserialize(challenge.payload.www_authenticate);
    } catch {
      throw new Error('MPP challenge has no valid WWW-Authenticate value');
    }
    this.requireTempoCharge(parsed);
    this.requireSupportedHeader(parsed);
    return {
      headers: { 'www-authenticate': Challenge.serialize(parsed) },
      contentType: 'application/json',
      body: Buffer.from('{"error":{"code":"PAYMENT_REQUIRED"}}'),
    };
  }

  receipt(payment: SettledIncomingPayment): ProtocolHttpResponse {
    return {
      headers: {
        'payment-receipt': Receipt.serialize(this.paymentReceipt(payment)),
        'cache-control': 'private',
      },
    };
  }

  mcpRequestMetadataKey(): string {
    return Mcp.credentialMetaKey;
  }

  mcpRootRequestMetadataKey(): string {
    return Mcp.credentialMetaKey;
  }

  mcpReceiptMetadataKey(): string {
    return Mcp.receiptMetaKey;
  }

  mcpChallenge(message: unknown, limit: number): ProtocolHttpResponse | undefined {
    this.boundedJson(message, limit, 'MPP MCP message');
    if (!this.isRecord(message)) return undefined;
    let errorData: unknown;
    if (this.isRecord(message.error) &&
        (message.error.code === Mcp.paymentRequiredCode ||
         message.error.code === Mcp.paymentVerificationFailedCode)) {
      errorData = message.error.data;
      if (!this.isRecord(errorData)) throw new Error('MPP MCP error omitted challenge data');
    }
    let resultData: unknown;
    if (this.isRecord(message.result) && this.isRecord(message.result._meta)) {
      resultData = message.result._meta[Mcp.paymentRequiredMetaKey];
      if (resultData !== undefined && !this.isRecord(resultData)) {
        throw new Error('MPP MCP result challenge is invalid');
      }
    }
    if (errorData === undefined && resultData === undefined) return undefined;
    if (errorData !== undefined && resultData !== undefined &&
        !isDeepStrictEqual(errorData, resultData)) {
      throw new Error('MPP MCP error challenge conflicts with result metadata');
    }
    const challenge = this.selectMcpChallenge(errorData ?? resultData);
    return {
      headers: { 'www-authenticate': Challenge.serialize(challenge) },
      contentType: 'application/json',
      body: Buffer.from('{"error":{"code":"PAYMENT_REQUIRED"}}'),
    };
  }

  mcpCredential(credential: PaymentCredential): McpMetadata {
    const delivery = this.credential(credential);
    const parsed = this.parseCredential(delivery.header_value, 'MPP credential');
    return { key: Mcp.credentialMetaKey, value: parsed };
  }

  presentedMcpCredential(meta: unknown, limit: number): unknown | undefined {
    if (!this.isRecord(meta)) throw new Error('MCP params._meta must be an object');
    const value = meta[Mcp.credentialMetaKey];
    if (value === undefined) return undefined;
    this.boundedJson(value, limit, 'MPP MCP credential');
    const parsed = this.parseMcpCredential(value);
    this.requireTempoCharge(parsed.challenge);
    return { authorization: Credential.serialize(parsed) };
  }

  mcpPaymentRequired(challenge: PaymentChallenge): McpPaymentRequired {
    const parsed = this.storedMcpChallenge(challenge);
    return {
      error: {
        code: Mcp.paymentRequiredCode,
        message: 'Payment Required',
        data: { httpStatus: 402, challenges: [parsed] },
      } satisfies Mcp.ErrorObject,
    };
  }

  mcpPaymentRejected(challenge: PaymentChallenge, message: string): McpPaymentRequired {
    const parsed = this.storedMcpChallenge(challenge);
    const error = new Errors.VerificationFailedError({ reason: message });
    return {
      error: {
        code: Mcp.paymentVerificationFailedCode,
        message: error.message,
        data: {
          httpStatus: 402,
          challenges: [parsed],
          problem: error.toProblemDetails(parsed.id),
        },
      } satisfies Mcp.ErrorObject,
    };
  }

  mcpReceipt(challenge: PaymentChallenge, payment: SettledIncomingPayment): McpMetadata {
    if (challenge.protocol !== this.name ||
        payment.intent.challenge_hash !== challenge.challenge_hash) {
      throw new Error('MPP MCP receipt challenge binding is invalid');
    }
    const stored = this.storedMcpChallenge(challenge);
    const receipt: Mcp.Receipt = {
      ...this.paymentReceipt(payment),
      challengeId: stored.id,
    };
    return { key: Mcp.receiptMetaKey, value: receipt };
  }

  mcpMerchantEvidence(meta: unknown, limit: number): MerchantEvidence | undefined {
    if (!this.isRecord(meta)) throw new Error('MCP result._meta must be an object');
    const value = meta[Mcp.receiptMetaKey];
    if (value === undefined) return undefined;
    this.boundedJson(value, limit, 'MPP MCP receipt metadata');
    const parsed = Receipt.Schema.safeParse(value);
    if (!parsed.success || !this.isRecord(value) || typeof value.challengeId !== 'string' ||
        !value.challengeId) {
      throw new Error('MPP MCP receipt metadata is invalid');
    }
    this.requireTempoReceipt(parsed.data);
    return {
      headerName: 'payment-receipt',
      headerValue: Receipt.serialize(parsed.data),
    };
  }

  a2aExtension(): string { return ''; }
  a2aChallenge(message: Record<string, unknown>, limit: number): ProtocolHttpResponse | undefined {
    return message.error !== undefined ? this.mcpChallenge(message, limit) : undefined;
  }
  a2aApplyCredential(message: Record<string, unknown>, credential: PaymentCredential): void {
    const meta = A2ACall.metadata(message, true), value = this.mcpCredential(credential);
    if (value.key in meta) throw new Error('A2A request already contains an MPP credential');
    meta[value.key] = value.value;
  }
  presentedA2aCredential(message: Record<string, unknown>, limit: number): unknown | undefined {
    const key = this.mcpRequestMetadataKey();
    if (key in A2ACall.object(A2ACall.object(message.params)._meta ?? {}) || key in A2ACall.metadata(message)) throw new Error('MPP A2A credentials require JSON-RPC root _meta');
    return this.presentedMcpCredential(A2ACall.metadata(message, true), limit);
  }
  a2aStripCredential(message: Record<string, unknown>): void { delete A2ACall.metadata(message, true)[this.mcpRequestMetadataKey()]; }
  a2aPaymentRequired(challenge: PaymentChallenge, _task: Task, rejected: boolean): McpPaymentRequired {
    return rejected ? this.mcpPaymentRejected(challenge, 'Payment verification failed') : this.mcpPaymentRequired(challenge);
  }
  a2aEvidence(message: Record<string, unknown>, limit: number): MerchantEvidence | undefined {
    return this.mcpMerchantEvidence(A2ACall.metadata(message, true), limit);
  }
  a2aInjectReceipt(message: Record<string, unknown>, challenge: PaymentChallenge, payment: SettledIncomingPayment): void {
    const meta = A2ACall.metadata(message, true), receipt = this.mcpReceipt(challenge, payment);
    if (receipt.key in meta) throw new Error('handler attempted to supply MPP receipt');
    meta[receipt.key] = receipt.value;
  }

  private storedMcpChallenge(challenge: PaymentChallenge): Challenge.Challenge {
    if (challenge.protocol !== this.name || !this.isRecord(challenge.payload)) {
      throw new Error('MPP challenge protocol or payload is invalid');
    }
    const parsed = Challenge.Schema.safeParse(challenge.payload.mcp_challenge);
    if (!parsed.success) throw new Error('MPP MCP challenge is invalid');
    this.requireTempoCharge(parsed.data);
    this.requireSupportedHeader(parsed.data);
    return parsed.data;
  }

  private selectMcpChallenge(data: unknown): Challenge.Challenge {
    if (!this.isRecord(data) || data.httpStatus !== 402 || !Array.isArray(data.challenges) ||
        data.challenges.length === 0) {
      throw new Error('MPP MCP challenge must carry HTTP 402 and at least one challenge');
    }
    for (const value of data.challenges) {
      const parsed = Challenge.Schema.safeParse(value);
      if (!parsed.success) continue;
      this.requireSupportedHeader(parsed.data);
      if (parsed.data.method === 'tempo' && parsed.data.intent === 'charge') return parsed.data;
    }
    throw new Error('MPP MCP challenge has no supported Tempo charge');
  }

  private parseCredential(value: string, label: string): Credential.Credential {
    try {
      return Credential.deserialize(value);
    } catch {
      throw new Error(`${label} is not a valid Payment authorization`);
    }
  }

  private parseMcpCredential(value: unknown): Credential.Credential {
    if (!this.isRecord(value) || !('challenge' in value) || !('payload' in value)) {
      throw new Error('MPP MCP credential is invalid');
    }
    try {
      return Credential.deserialize(Credential.serialize(value as Credential.Credential));
    } catch {
      throw new Error('MPP MCP credential is invalid');
    }
  }

  private parseReceipt(value: string, label: string): Receipt.Receipt {
    try {
      return Receipt.deserialize(value);
    } catch {
      throw new Error(`${label} is invalid`);
    }
  }

  private paymentReceipt(payment: SettledIncomingPayment): Receipt.Receipt {
    const receipt = payment.receipt;
    if (receipt.protocol !== this.name || !receipt.tx_hash || receipt.confirmed_at <= 0) {
      throw new Error('confirmed MPP receipt omitted transaction or confirmation time');
    }
    return Receipt.from({
      status: 'success',
      method: 'tempo',
      timestamp: new Date(receipt.confirmed_at * 1000).toISOString(),
      reference: receipt.tx_hash,
      externalId: payment.intent.id,
    });
  }

  private requireTempoCharge(challenge: Challenge.Challenge): void {
    if (challenge.method !== 'tempo' || challenge.intent !== 'charge') {
      throw new Error('MPP supports only the implemented Tempo charge flow');
    }
  }

  private requireSupportedHeader(challenge: Challenge.Challenge): void {
    if (challenge.header !== undefined &&
        challenge.header.toLowerCase() !== 'payment-authorization') {
      throw new Error('MPP challenge selected an unsupported credential header');
    }
  }

  private requireTempoReceipt(receipt: Receipt.Receipt): void {
    if (receipt.status !== 'success' || receipt.method !== 'tempo' || !receipt.reference) {
      throw new Error('MPP Payment-Receipt is not a successful Tempo receipt');
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
}
