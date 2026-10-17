import type { PayeeConfig } from '../config/config.js';
import { PaymentCredential, required, type PaymentChallenge, type PaymentReceipt } from '../model/base.js';
import {
  operationContext,
  validatePayeeOperation,
  validateResolveOfferRequest,
  type CreateChallengeRequest,
  type PayeeOperation,
  type PaymentOffer,
  type PaymentVerification,
  type ProtectionOutcome,
  type ResolveOfferRequest,
  type SettlePaymentRequest,
  type SettledIncomingPayment,
  type VerifyPaymentRequest,
} from '../model/payee.js';
import type { VaultTransport } from '../transport/http.js';
import { ExpressPayeeMiddleware, type ExpressPayeeRoute } from './express.js';
import { StandardPayeeMiddleware } from './standard.js';

export class InvalidPaymentError extends Error {}

export class PayeeMiddleware {
  readonly config: Required<PayeeConfig>;

  constructor(private readonly transport: VaultTransport, config: PayeeConfig = {}) {
    if (!transport) throw new Error('AgentVault transport is not configured');
    this.config = { maxBodyBytes: config.maxBodyBytes ?? 1024 * 1024 };
    if (this.config.maxBodyBytes <= 0) throw new Error('payee middleware body limit must be positive');
  }

  express(routes: readonly ExpressPayeeRoute[]): ExpressPayeeMiddleware {
    return new ExpressPayeeMiddleware(this, routes, this.config.maxBodyBytes);
  }

  standard(): StandardPayeeMiddleware {
    return new StandardPayeeMiddleware(this);
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
    const challenge = await this.challenge(operation);
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
      throw new InvalidPaymentError('AgentVault rejected the presented payment');
    }
    if (!verification.verification_ref) {
      throw new Error('valid AgentVault verification omitted verification_ref');
    }
    validateVerificationBinding(operation, challenge, verification);
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
    return { outcome: 'authorized', payment };
  }

  resolveOffer(request: ResolveOfferRequest): Promise<PaymentOffer> {
    validateResolveOfferRequest(request);
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
