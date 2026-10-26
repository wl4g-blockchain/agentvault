import {
  assertRunId, PaymentCredential, required, type AssetIdentity, type OperationContext,
  type PaymentCredentialWire, type PaymentIntent,
} from './base.js';
import type { ApprovalRequest } from './approval.js';

export type AuthorizationOutcome = 'authorized' | 'requires_approval' | 'denied';

export interface PaymentRequired {
  protocol_hint?: string;
  origin: string;
  resource: string;
  content_type: string;
  headers?: Record<string, string>;
  body: string;
  expires_at?: string;
}

export interface AuthorizePaymentRequest extends OperationContext {
  challenge: PaymentRequired;
}

export interface AuthorizePaymentResponse {
  authorization: {
    outcome: AuthorizationOutcome;
    intent_id: string;
    approval_id?: string;
    reserved_amount?: string;
    reason?: string;
  };
  intent: PaymentIntent;
  credential?: PaymentCredentialWire;
  approval?: ApprovalRequest;
}

export interface ProtocolEvidence {
  header_name: string;
  header_value: string;
}

export interface ReportOutboundResponseRequest extends OperationContext {
  protocol: string;
  evidence: ProtocolEvidence;
}

export interface ReportOutboundResponseResponse {
  intent: PaymentIntent;
  accepted: boolean;
  replayed: boolean;
}

export class PayerRequestContext {
  constructor(public readonly runId: string) {
    if (!runId.trim()) throw new Error('runId cannot be blank');
    assertRunId(runId);
  }

  operationContext(scenarioId: string): OperationContext {
    return { scenario_id: scenarioId, run_id: this.runId };
  }
}

export interface PayerHttpRequest {
  method: string;
  url: string;
  origin: string;
  headers: Headers;
  body?: Uint8Array;
  followRedirects: boolean;
}

export interface PayerHttpResponse {
  status: number;
  headers: Headers;
  body?: Uint8Array;
}

export interface AuthorizedPayment {
  intent: PaymentIntent;
  credential: PaymentCredential;
}

export interface BalanceQuery {
  scenarioId: string;
  walletId: string;
  asset: AssetIdentity;
}

export interface PaymentBalance {
  scenario_id: string;
  wallet_id: string;
  asset: AssetIdentity;
  confirmed_inbound: string;
  confirmed_outbound: string;
  reserved_outbound: string;
  available?: string;
  as_of: number;
}

export function validateBalanceQuery(query: BalanceQuery): void {
  required('scenarioId', query.scenarioId);
  required('walletId', query.walletId);
  required('asset.network', query.asset.network);
  required('asset.asset', query.asset.asset);
  if (!Number.isInteger(query.asset.decimals) || query.asset.decimals < 0 || query.asset.decimals > 255) {
    throw new Error('asset.decimals must be in 0..255');
  }
}
