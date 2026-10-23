import {
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
  intent_id: string;
  verification_ref: string;
}

export interface SettledIncomingPayment {
  intent: PaymentIntent;
  receipt: PaymentReceipt;
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
  | { outcome: 'authorized'; challenge: PaymentChallenge; payment: SettledIncomingPayment };

export function validateResolveOfferRequest(request: ResolveOfferRequest): void {
  required('scenarioId', request.scenarioId);
  required('resource', request.resource);
  if (request.protocol !== undefined) required('protocol', request.protocol);
}

export function validatePayeeOperation(operation: PayeeOperation): void {
  for (const [name, value] of [
    ['scenarioId', operation.scenarioId], ['runId', operation.runId],
    ['offerId', operation.offerId],
    ['resource', operation.resource], ['origin', operation.origin],
    ['protocol', operation.protocol], ['requestHash', operation.requestHash],
  ]) {
    required(name!, value!);
  }
}

export function operationContext(operation: PayeeOperation): OperationContext {
  return {
    scenario_id: operation.scenarioId,
    run_id: operation.runId,
  };
}
