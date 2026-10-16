export type Amount = string

export type AssetIdentity = {
  network: string
  asset: string
  token_contract?: string
  decimals: number
}

export type PaymentDirection = 'inbound' | 'outbound'
export type PaymentStatus =
  | 'created' | 'pending_approval' | 'authorized' | 'signed' | 'submitted'
  | 'pending_settlement' | 'settled' | 'failed' | 'expired' | 'unknown'

export type ReservationStatus = 'unreserved' | 'reserved' | 'consumed' | 'released'

export type PaymentIntent = {
  id: string
  scenario_id: string
  run_id: string
  wallet_id?: string
  policy_id?: string
  offer_id?: string
  direction: PaymentDirection
  protocol: string
  asset: AssetIdentity
  recipient: string
  amount: Amount
  settled_amount: Amount
  resource: string
  challenge_hash?: string
  idempotency_key: string
  request_hash: string
  status: PaymentStatus
  reservation_status: ReservationStatus
  reserved_amount: Amount
  tx_hash?: string
  settlement_ref?: string
  next_reconcile_at?: number
  created_at: number
  updated_at: number
  expires_at?: number
  settled_at?: number
}

export type PaymentReceipt = {
  intent_id: string
  scenario_id: string
  direction: PaymentDirection
  protocol: string
  asset: AssetIdentity
  recipient: string
  payer?: string
  requested_amount: Amount
  settled_amount: Amount
  status: PaymentStatus
  tx_hash?: string
  settlement_ref?: string
  evidence_hash: string
  confirmed_at: number
}

export type ApprovalStatus = 'pending' | 'approved' | 'rejected' | 'expired' | 'consumed'

export type ApprovalRequest = {
  id: string
  intent_id: string
  requested_amount: Amount
  approved_amount?: Amount
  requested_by: string
  approved_by?: string
  status: ApprovalStatus
  intent_hash: string
  grant_jti?: string
  grant_claims_hash?: string
  expires_at: number
  consumed_at?: number
  created_at: number
  approved_at?: number
}

export type ApprovalPage = { items: ApprovalRequest[]; total: number }

export type PaymentBalance = {
  scenario_id: string
  wallet_id: string
  asset: AssetIdentity
  confirmed_inbound: Amount
  confirmed_outbound: Amount
  reserved_outbound: Amount
  available?: Amount
  as_of: number
}

export type PaymentOffer = {
  id: string
  scenario_id: string
  policy_id: string
  resource: string
  protocols: string[]
  asset: AssetIdentity
  amount: Amount
  wallet_id: string
  enabled: boolean
  created_at: number
  updated_at: number
}

export type BalanceQuery = {
  scenario_id: string
  wallet_id: string
  network: string
  asset: string
  token_contract?: string
  decimals: number
}

export type ApprovalDecisionInput = {
  scenarioId: string
  runId: string
  approvalId: string
  decision: 'approve' | 'reject'
  approvedAmount?: Amount
  reason?: string
}

export type ManagementResource = 'clients' | 'scenarios' | 'policies' | 'wallets'
export type ManagementPage = { items: unknown[]; total: number }

export type ApiCapabilities = {
  paymentLookup: true
  receiptLookup: true
  balanceLookup: true
  pendingApprovalRead: true
  approvalDecision: true
  offerResolution: true
  managementRead: true
  managementMutation: false
  ledgerListing: false
  reconciliationControl: false
}
