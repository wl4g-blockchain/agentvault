import type {
  ApiCapabilities,
  ApprovalDecisionInput,
  ApprovalPage,
  ApprovalRequest,
  BalanceQuery,
  ManagementPage,
  ManagementResource,
  PaymentBalance,
  PaymentIntent,
  PaymentOffer,
  PaymentReceipt,
} from './types'

export const API_ROOT = '/api/v1'
export const MANAGEMENT_ROOT = '/agentvault-management'

export const capabilities: ApiCapabilities = {
  paymentLookup: true,
  receiptLookup: true,
  balanceLookup: true,
  pendingApprovalRead: true,
  approvalDecision: true,
  offerResolution: true,
  managementRead: true,
  managementMutation: false,
  ledgerListing: false,
  reconciliationControl: false,
}

const sensitiveKey = /(?:^|_)(?:authorization|bearer|access_token|refresh_token|secret|private_key|signature|credential|grant(?:_token)?)(?:$|_)/i

export function redactForDisplay(value: unknown): unknown {
  if (Array.isArray(value)) return value.map(redactForDisplay)
  if (value && typeof value === 'object') {
    return Object.fromEntries(Object.entries(value).map(([key, item]) => [
      key,
      sensitiveKey.test(key) ? '[REDACTED]' : redactForDisplay(item),
    ]))
  }
  return value
}

export class ApiError extends Error {
  constructor(public readonly status: number, public readonly code?: string) {
    super(code ? `AgentVault request failed (${status}, ${code})` : `AgentVault request failed (${status})`)
    this.name = 'ApiError'
  }
}

async function request<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await fetch(`${API_ROOT}${path}`, {
    ...init,
    credentials: 'same-origin',
    headers: {
      accept: 'application/json',
      ...(init?.body ? { 'content-type': 'application/json' } : {}),
      ...init?.headers,
    },
  })
  if (!response.ok) {
    const body = await response.json().catch(() => undefined) as { code?: unknown } | undefined
    const code = typeof body?.code === 'string' && /^[A-Z0-9_:-]{1,64}$/i.test(body.code) ? body.code : undefined
    throw new ApiError(response.status, code)
  }
  if (response.status === 204) return undefined as T
  return response.json() as Promise<T>
}

function query(values: Record<string, string | number | undefined>): string {
  const params = new URLSearchParams()
  Object.entries(values).forEach(([key, value]) => {
    if (value !== undefined && value !== '') params.set(key, String(value))
  })
  return params.toString()
}

export const paymentApi = {
  getPayment(scenarioId: string, intentId: string) {
    return request<PaymentIntent>(`/payments/${encodeURIComponent(intentId)}?${query({ scenario_id: scenarioId })}`)
  },

  getReceipt(scenarioId: string, intentId: string) {
    return request<PaymentReceipt>(`/payments/${encodeURIComponent(intentId)}/receipt?${query({ scenario_id: scenarioId })}`)
  },

  getBalance(input: BalanceQuery) {
    return request<PaymentBalance>(`/payments/balance?${query({
      scenario_id: input.scenario_id,
      wallet_id: input.wallet_id,
      network: input.network,
      asset: input.asset,
      token_contract: input.token_contract,
      decimals: input.decimals,
    })}`)
  },
}

export const approvalApi = {
  listPending(scenarioId: string, offset = 0, limit = 100) {
    return request<ApprovalPage>(`/approvals?${query({ scenario_id: scenarioId, status: 'pending', offset, limit })}`)
  },

  get(scenarioId: string, approvalId: string) {
    return request<ApprovalRequest>(`/approvals/${encodeURIComponent(approvalId)}?${query({ scenario_id: scenarioId })}`)
  },

  decide(input: ApprovalDecisionInput) {
    return request<ApprovalRequest>(`/approvals/${encodeURIComponent(input.approvalId)}/decision`, {
      method: 'POST',
      body: JSON.stringify({
        scenario_id: input.scenarioId,
        run_id: input.runId,
        approval_id: input.approvalId,
        decision: input.decision,
        ...(input.approvedAmount ? { approved_amount: input.approvedAmount } : {}),
        ...(input.reason ? { reason: input.reason } : {}),
      }),
    })
  },
}

export const offerApi = {
  resolve(scenarioId: string, resource: string, protocol?: string) {
    return request<PaymentOffer>('/offers/resolve', {
      method: 'POST',
      body: JSON.stringify({
        scenario_id: scenarioId,
        resource,
        ...(protocol ? { protocol } : {}),
      }),
    })
  },
}

export const managementApi = {
  list(resource: ManagementResource, parentId?: string, offset = 0, limit = 100) {
    const parent = resource === 'scenarios'
      ? { client_id: parentId }
      : resource === 'policies'
        ? { scenario_id: parentId }
        : {}
    return request<ManagementPage>(`/admin/${resource}?${query({ ...parent, offset, limit })}`)
  },

  async health(signal?: AbortSignal) {
    const probe = async (path: 'healthz' | 'readyz') => {
      const response = await fetch(`${MANAGEMENT_ROOT}/${path}`, {
        credentials: 'same-origin',
        headers: { accept: 'text/plain' },
        signal,
      })
      return { ok: response.ok, status: response.status, message: (await response.text()).slice(0, 80) }
    }
    const [liveness, readiness] = await Promise.all([probe('healthz'), probe('readyz')])
    return { liveness, readiness, observedAt: Date.now() }
  },
}
