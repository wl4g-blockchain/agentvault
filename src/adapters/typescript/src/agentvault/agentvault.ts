import type { PayeeConfig, PayerConfig } from './config/config.js';
import type {
  ApprovalPage,
  ApprovalQuery,
  ApprovalRequest,
  SubmitApprovalDecisionRequest,
} from './model/approval.js';
import { validateApprovalDecision, validateApprovalQuery } from './model/approval.js';
import { required } from './model/base.js';
import { PayeeMiddleware } from './payee/payee.js';
import { PayerMiddleware } from './payer/payer.js';
import type { VaultTransport } from './transport/transport.js';

/** The only SDK facade: construct role middleware from one configured transport. */
export class AgentVault {
  constructor(private readonly transport: VaultTransport) {
    if (!transport) throw new Error('AgentVault transport is not configured');
  }

  payer(config: PayerConfig): PayerMiddleware {
    return new PayerMiddleware(this.transport, config);
  }

  payee(config: PayeeConfig = {}): PayeeMiddleware {
    return new PayeeMiddleware(this.transport, config);
  }

  getPendingApprovals(query: ApprovalQuery): Promise<ApprovalPage> {
    validateApprovalQuery(query);
    return this.transport.request(
      'GET',
      `/api/v1/approvals?scenario_id=${encodeURIComponent(query.scenario_id)}&status=pending`,
    );
  }

  submitApprovalDecision(request: SubmitApprovalDecisionRequest): Promise<ApprovalRequest> {
    validateApprovalDecision(request);
    return this.transport.request(
      'POST',
      `/api/v1/approvals/${encodeURIComponent(request.approval_id)}/decision`,
      request,
    );
  }

  getApprovalStatus(scenarioId: string, approvalId: string): Promise<ApprovalRequest> {
    required('scenarioId', scenarioId);
    required('approvalId', approvalId);
    return this.transport.request(
      'GET',
      `/api/v1/approvals/${encodeURIComponent(approvalId)}?scenario_id=${encodeURIComponent(scenarioId)}`,
    );
  }
}

export * from './config/config.js';
export * from './model/approval.js';
export * from './model/base.js';
export * from './model/payee.js';
export * from './model/payer.js';
export * from './payee/express.js';
export * from './payee/payee.js';
export * from './payee/standard.js';
export * from './payer/payer.js';
export * from './payer/standard.js';
export * from './transport/grpc.js';
export * from './transport/mqtt.js';
export * from './transport/transport.js';
