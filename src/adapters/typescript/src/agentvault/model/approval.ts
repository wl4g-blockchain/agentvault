import { assertIdempotencyKey, required, type OperationContext } from './base.js';

export type ApprovalStatus = 'pending' | 'approved' | 'rejected' | 'expired' | 'consumed';
export type ApprovalDecisionKind = 'approve' | 'reject';

export interface ApprovalRequest {
  id: string;
  intent_id: string;
  requested_amount: string;
  approved_amount?: string;
  requested_by: string;
  approved_by?: string;
  status: ApprovalStatus;
  intent_hash: string;
  grant_jti?: string;
  grant_claims_hash?: string;
  expires_at: number;
  created_at: number;
  approved_at?: number;
  consumed_at?: number;
}

export interface ApprovalQuery {
  scenario_id: string;
}

export interface ApprovalPage {
  items: ApprovalRequest[];
  total: number;
}

export interface SubmitApprovalDecisionRequest extends OperationContext {
  approval_id: string;
  decision: ApprovalDecisionKind;
  approved_amount?: string;
  reason?: string;
}

export function validateApprovalQuery(query: ApprovalQuery): void {
  required('scenario_id', query.scenario_id);
}

export function validateApprovalDecision(request: SubmitApprovalDecisionRequest): void {
  required('scenario_id', request.scenario_id);
  required('approval_id', request.approval_id);
  assertIdempotencyKey(request.idempotency_key);
  if (request.decision === 'approve') required('approved_amount', request.approved_amount ?? '');
  if (request.decision === 'reject' && request.approved_amount !== undefined) {
    throw new Error('approved_amount must be omitted when rejecting');
  }
}
