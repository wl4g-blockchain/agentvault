import { A2APayeeMiddleware } from './a2a.js';
import type { PayeeConfig } from '../config/config.js';
import { PaymentCredential, required, type PaymentChallenge, type PaymentReceipt } from '../model/base.js';
import {
  operationContext,
  validatePayeeOperation,
  validateResolvePriceRequest,
  type CreateChallengeRequest,
  type PayeeOperation,
  type PaymentPrice,
  type PaymentVerification,
  type ProtectionOutcome,
  type ResolvePriceRequest,
  type SettlePaymentRequest,
  type SettledIncomingPayment,
  type VerifyPaymentRequest,
} from '../model/payee.js';
import type { VaultTransport } from '../transport/transport.js';
import {
  PaymentProtocolFactory,
  type McpMetadata,
  type McpPaymentRequired,
  type PaymentProtocol,
  type ProtocolRegistry,
} from '../protocol/adapter.js';
import { ExpressPayeeMiddleware, type ExpressPayeeRoute } from './express.js';
import { StandardPayeeMiddleware } from './standard.js';

export class InvalidPaymentError extends Error {
  constructor(public readonly challenge: PaymentChallenge) {
    super('AgentVault rejected the presented payment');
  }
}

export class PayeeMiddleware {
  readonly config: Required<PayeeConfig>;
  private readonly protocols: ProtocolRegistry;

  a2a(): A2APayeeMiddleware { return new A2APayeeMiddleware(this, this.protocols); }

  constructor(private readonly transport: VaultTransport, config: PayeeConfig = {}) {
    if (!transport) throw new Error('AgentVault transport is not configured');
    this.config = {
      maxBodyBytes: config.maxBodyBytes ?? 1024 * 1024,
      protocols: config.protocols ?? ['x402', 'mpp'],
    };
    if (this.config.maxBodyBytes <= 0) throw new Error('payee middleware body limit must be positive');
    this.protocols = new PaymentProtocolFactory().create(this.config.protocols);
  }

  express(routes: readonly ExpressPayeeRoute[]): ExpressPayeeMiddleware {
    return new ExpressPayeeMiddleware(this, routes, this.config.maxBodyBytes);
  }

  standard(): StandardPayeeMiddleware {
    return new StandardPayeeMiddleware(this);
  }

  protocol(name: string): PaymentProtocol {
    return this.protocols.get(name);
  }

  credentialHeaderNames(): readonly string[] {
    return this.protocols.credentialHeaderNames();
  }

  merchantEvidenceHeaderNames(): readonly string[] {
    return this.protocols.merchantEvidenceHeaderNames();
  }

  presentedCredential(protocol: string, headers: Headers): unknown | undefined {
    this.protocols.get(protocol);
    const [selected, payload] = this.protocols.presentedCredential(
      headers,
      this.config.maxBodyBytes,
    );
    if (selected !== undefined && selected !== protocol) {
      throw new Error('payment credential protocol does not match the protected route');
    }
    return payload;
  }

  presentedMcpCredential(protocol: string, meta: unknown): unknown | undefined {
    const [selected, payload] = this.protocols.presentedMcpCredential(
      meta,
      this.config.maxBodyBytes,
    );
    if (selected !== undefined && selected !== protocol) {
      throw new Error('MCP payment credential protocol does not match the protected route');
    }
    return payload;
  }

  mcpRequestMetadataKeys(): readonly string[] {
    return this.protocols.mcpRequestMetadataKeys();
  }

  mcpRootRequestMetadataKeys(): readonly string[] {
    return this.protocols.mcpRootRequestMetadataKeys();
  }

  mcpReceiptMetadataKeys(): readonly string[] {
    return this.protocols.mcpReceiptMetadataKeys();
  }

  merchantEvidence(
    headers: Headers,
  ): readonly [string | undefined, import('../protocol/adapter.js').MerchantEvidence | undefined] {
    return this.protocols.merchantEvidence(headers);
  }

  mcpPaymentRequired(
    challenge: PaymentChallenge,
    rejected = false,
  ): McpPaymentRequired {
    const adapter = this.protocols.mcp(challenge.protocol);
    return rejected
      ? adapter.mcpPaymentRejected(challenge, 'AgentVault rejected the presented payment')
      : adapter.mcpPaymentRequired(challenge);
  }

  mcpReceipt(
    challenge: PaymentChallenge,
    payment: SettledIncomingPayment,
  ): McpMetadata {
    return this.protocols.mcp(payment.receipt.protocol).mcpReceipt(challenge, payment);
  }

  mcpMerchantEvidence(
    meta: unknown,
  ): readonly [string | undefined, import('../protocol/adapter.js').MerchantEvidence | undefined] {
    return this.protocols.mcpMerchantEvidence(meta, this.config.maxBodyBytes);
  }

  async challenge(operation: PayeeOperation): Promise<PaymentChallenge> {
    validatePayeeOperation(operation);
    const context = operationContext(operation);
    const request: CreateChallengeRequest = {
      ...context,
      offer_id: operation.offerId,
      resource: operation.resource,
      origin: operation.origin,
      protocol: operation.protocol,
      request_hash: operation.requestHash,
    };
    const challenge = await this.transport.request<PaymentChallenge>(
      'POST',
      '/api/v1/payee/challenges',
      request,
    );
    validateChallengeBinding(operation, challenge);
    return challenge;
  }

  async protect(operation: PayeeOperation): Promise<ProtectionOutcome> {
    sdkEvent('agentvault.sdk.payee.intercepted', {
      runId: operation.runId, scenarioId: operation.scenarioId,
    });
    const challenge = await this.challenge(operation);
    sdkEvent('agentvault.sdk.payee.challenge_created', {
      runId: operation.runId, challengeHash: challenge.challenge_hash,
    });
    if (operation.credentialPayload === undefined) {
      return { outcome: 'payment_required', challenge };
    }
    const context = operationContext(operation);
    const credential = new PaymentCredential(
      operation.protocol,
      challenge.challenge_hash,
      'untrusted',
      operation.credentialPayload,
    );
    const verifyRequest: VerifyPaymentRequest = {
      ...context,
      challenge_hash: challenge.challenge_hash,
      request_hash: operation.requestHash,
      credential: credential.toJSON(),
    };
    const verification = await this.transport.request<PaymentVerification>(
      'POST',
      '/api/v1/payee/verify',
      verifyRequest,
    );
    if (!verification.valid) {
      throw new InvalidPaymentError(challenge);
    }
    if (!verification.verification_ref) {
      throw new Error('valid AgentVault verification omitted verification_ref');
    }
    validateVerificationBinding(operation, challenge, verification);
    sdkEvent('agentvault.sdk.payee.verified', {
      runId: operation.runId, intentId: verification.intent.id,
    });
    const settlement = operationContext(operation);
    const settleRequest: SettlePaymentRequest = {
      ...settlement,
      intent_id: verification.intent.id,
      verification_ref: verification.verification_ref,
    };
    const payment = await this.transport.request<SettledIncomingPayment>(
      'POST',
      '/api/v1/payee/settle',
      settleRequest,
    );
    validateSettlementBinding(operation, verification, payment);
    sdkEvent('agentvault.sdk.payee.settled', {
      runId: operation.runId, intentId: payment.intent.id,
    });
    return { outcome: 'authorized', challenge, payment };
  }

  resolvePrice(request: ResolvePriceRequest): Promise<PaymentPrice> {
    validateResolvePriceRequest(request);
    const body: Record<string, string> = { scenario_id: request.scenarioId, resource: request.resource };
    if (request.protocol) body.protocol = request.protocol;
    return this.transport.request('POST', '/api/v1/offers/resolve', body);
  }

  getReceipt(scenarioId: string, intentId: string): Promise<PaymentReceipt> {
    required('scenarioId', scenarioId);
    required('intentId', intentId);
    return this.transport.request(
      'GET',
      `/api/v1/payments/${encodeURIComponent(intentId)}/receipt?scenario_id=${encodeURIComponent(scenarioId)}`,
    );
  }
}

function sdkEvent(event: string, fields: Record<string, unknown>): void {
  console.info(JSON.stringify({ event, ...fields }));
}

function validateChallengeBinding(operation: PayeeOperation, challenge: PaymentChallenge): void {
  if (challenge.scenario_id !== operation.scenarioId || challenge.offer_id !== operation.offerId ||
      challenge.resource !== operation.resource || challenge.protocol !== operation.protocol ||
      !challenge.asset?.network || !challenge.asset.asset || !challenge.amount || !challenge.recipient ||
      !challenge.nonce || !challenge.challenge_hash) {
    throw new Error('AgentVault challenge is not bound to the protected operation');
  }
}

function validateVerificationBinding(
  operation: PayeeOperation,
  challenge: PaymentChallenge,
  verification: PaymentVerification,
): void {
  const intent = verification.intent;
  if (intent.direction !== 'inbound' || intent.scenario_id !== operation.scenarioId ||
      intent.offer_id !== operation.offerId || intent.resource !== operation.resource ||
      intent.protocol !== operation.protocol || intent.challenge_hash !== challenge.challenge_hash ||
      intent.request_hash !== operation.requestHash || !sameAsset(intent.asset, challenge.asset) ||
      intent.amount !== challenge.amount || intent.recipient !== challenge.recipient) {
    throw new Error('AgentVault verification is not bound to the protected operation');
  }
}

function validateSettlementBinding(
  operation: PayeeOperation,
  verification: PaymentVerification,
  payment: SettledIncomingPayment,
): void {
  const intent = payment.intent;
  const verified = verification.intent;
  const receipt = payment.receipt;
  if (intent.id !== verified.id || intent.direction !== 'inbound' ||
      intent.scenario_id !== verified.scenario_id || intent.offer_id !== verified.offer_id ||
      intent.resource !== verified.resource || intent.protocol !== verified.protocol ||
      intent.challenge_hash !== verified.challenge_hash || intent.request_hash !== verified.request_hash ||
      !sameAsset(intent.asset, verified.asset) || intent.amount !== verified.amount ||
      intent.recipient !== verified.recipient || intent.status !== 'settled' ||
      receipt.intent_id !== intent.id || receipt.scenario_id !== operation.scenarioId ||
      receipt.direction !== 'inbound' || receipt.protocol !== operation.protocol ||
      !sameAsset(receipt.asset, intent.asset) || receipt.recipient !== intent.recipient ||
      receipt.requested_amount !== intent.amount || receipt.settled_amount !== intent.settled_amount ||
      receipt.status !== 'settled') {
    throw new Error('invalid AgentVault settlement response');
  }
}

function sameAsset(left: import('../model/base.js').AssetIdentity,
                   right: import('../model/base.js').AssetIdentity): boolean {
  return left.network === right.network && left.asset === right.asset &&
    left.token_contract === right.token_contract && left.decimals === right.decimals;
}
