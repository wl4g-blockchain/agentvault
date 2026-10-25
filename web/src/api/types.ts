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

export type Page<T> = { items: T[]; total: number }
export type PaymentPage = Page<PaymentIntent>

export type PaymentListInput = {
  scenario_id?: string
  wallet_id?: string
  run_id?: string
  direction?: PaymentDirection
  status?: PaymentStatus
  offset?: number
  limit?: number
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

export type LedgerEntryType = 'payment' | 'fee' | 'refund' | 'deposit' | 'transfer' | 'reversal'

export type PaymentLedgerEntry = {
  id: string
  intent_id?: string
  scenario_id?: string
  wallet_id: string
  entry_type: LedgerEntryType
  direction: PaymentDirection
  asset: AssetIdentity
  amount: Amount
  posting_key: string
  tx_hash?: string
  settlement_ref?: string
  effect_ref?: string
  transfer_ref?: string
  reversal_of?: string
  effective_at: number
  created_at: number
}

export type LedgerPage = Page<PaymentLedgerEntry>
export type LedgerListInput = {
  scenario_id?: string
  wallet_id?: string
  intent_id?: string
  direction?: PaymentDirection
  network?: string
  asset?: string
  offset?: number
  limit?: number
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

export type PaymentClient = {
  id: string
  name: string
  auth_principal: string
  enabled: boolean
  created_at: number
  updated_at: number
}

export type PaymentScenario = {
  id: string
  client_id: string
  name: string
  enabled: boolean
  created_at: number
  updated_at: number
}

export type Wallet = {
  id: string
  algorithm: string
  address: string
  public_key: string
  created_at: number
}

export type WalletImportSession = {
  session_id: string
  server_public_key: string
  expires_at: number
}

export type EncryptedWalletPrivateKey = {
  session_id: string
  ciphertext: string
}

export type CreateWalletInput = {
  id: string
  algorithm: 'secp256k1'
  enabled: boolean
  encrypted_private_key: EncryptedWalletPrivateKey
}

export type PaymentPolicyLimits = {
  max_per_payment: Amount
  max_per_day: Amount
  approval_threshold?: Amount
  approval_enabled: boolean
  approval_overage?: Amount
}

export type PaymentPolicy = {
  id: string
  scenario_id: string
  wallet_ids: string[]
  protocols: string[]
  networks: string[]
  assets: AssetIdentity[]
  budget_unit: { asset: string; decimals: number }
  allowed_recipients: string[]
  allowed_origins: string[]
  inbound_limits?: PaymentPolicyLimits
  outbound_limits?: PaymentPolicyLimits
  enabled: boolean
  created_at: number
  updated_at: number
}

export type ScenarioConfiguration = { scenario: PaymentScenario; policy: PaymentPolicy }
export type ScenarioConfigurationInput = {
  id: string
  client_id: string
  name: string
  policy: Omit<PaymentPolicy, 'scenario_id' | 'created_at' | 'updated_at'>
}

export type ManagementResource = 'clients' | 'scenarios' | 'policies' | 'wallets'
export type ManagementRecord = PaymentClient | PaymentScenario | PaymentPolicy | Wallet
export type ManagementPage = Page<ManagementRecord>

export type ApiCapabilities = {
  paymentLookup: true
  receiptLookup: true
  balanceLookup: true
  pendingApprovalRead: true
  approvalDecision: true
  offerResolution: true
  managementRead: true
  managementMutation: true
  paymentListing: true
  ledgerListing: true
  reconciliationControl: false
}
