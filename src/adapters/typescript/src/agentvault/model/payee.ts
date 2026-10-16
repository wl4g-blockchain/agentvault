import {
  assertIdempotencyKey,
  required,
  type AssetIdentity,
  type OperationContext,
  type PaymentChallenge,
  type PaymentCredentialWire,
  type PaymentIntent,
  type PaymentReceipt,
} from './base.js';

export interface ResolveOfferRequest {
  scenarioId: string;
  resource: string;
  protocol?: string;
}

export interface PaymentOffer {
  id: string;
  scenario_id: string;
  resource: string;
  protocols: string[];
  asset: AssetIdentity;
  amount: string;
  wallet_id: string;
  enabled: boolean;
  created_at: number;
  updated_at: number;
}

export interface PayeeOperation {
  scenarioId: string;
  runId: string;
  executionKey: string;
  offerId: string;
  resource: string;
  origin: string;
  protocol: string;
  requestHash: string;
  credentialPayload?: unknown;
}

export interface CreateChallengeRequest extends OperationContext {
  offer_id: string;
  resource: string;
  origin: string;
  protocol: string;
  request_hash: string;
}

export interface VerifyPaymentRequest extends OperationContext {
  challenge_hash: string;
  request_hash: string;
  credential: PaymentCredentialWire;
}

export interface PaymentVerification {
  intent: PaymentIntent;
  valid: boolean;
  verification_ref?: string;
}

export interface SettlePaymentRequest extends OperationContext {
  execution_key: string;
  intent_id: string;
  verification_ref: string;
}

export interface IncomingPayment {
  scenario_id: string;
  run_id: string;
  offer_id: string;
  resource: string;
  idempotency_key: string;
  request_hash: string;
  credential: PaymentCredentialWire;
}

export interface SettledIncomingPayment {
  intent: PaymentIntent;
  receipt: PaymentReceipt;
  execution_key: string;
}

export interface HttpProtectionRequest extends OperationContext {
  resource: string;
  origin: string;
  credential?: PaymentCredentialWire;
  request_hash: string;
}

export interface McpProtectionRequest extends OperationContext {
  tool: string;
  arguments_hash: string;
  credential?: PaymentCredentialWire;
}

export type ProtectionOutcome =
  | { outcome: 'payment_required'; challenge: PaymentChallenge }
  | { outcome: 'authorized'; payment: SettledIncomingPayment };

export function validateResolveOfferRequest(request: ResolveOfferRequest): void {
  required('scenarioId', request.scenarioId);
  required('resource', request.resource);
  if (request.protocol !== undefined) required('protocol', request.protocol);
}

export function validatePayeeOperation(operation: PayeeOperation): void {
  for (const [name, value] of [
    ['scenarioId', operation.scenarioId], ['runId', operation.runId],
    ['executionKey', operation.executionKey], ['offerId', operation.offerId],
    ['resource', operation.resource], ['origin', operation.origin],
    ['protocol', operation.protocol], ['requestHash', operation.requestHash],
  ]) {
    required(name!, value!);
  }
  if (!/^[A-Za-z0-9_.-]{1,96}$/.test(operation.executionKey)) throw new Error('invalid executionKey');
  assertIdempotencyKey(`${operation.executionKey}.challenge`);
}

export function operationContext(operation: PayeeOperation, suffix: 'challenge' | 'settle'): OperationContext {
  return {
    scenario_id: operation.scenarioId,
    run_id: operation.runId,
    idempotency_key: `${operation.executionKey}.${suffix}`,
  };
}
