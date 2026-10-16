import { inspect } from 'node:util';

export type PaymentDirection = 'inbound' | 'outbound';
export type PaymentStatus =
  | 'created'
  | 'pending_approval'
  | 'authorized'
  | 'signed'
  | 'submitted'
  | 'pending_settlement'
  | 'settled'
  | 'failed'
  | 'expired'
  | 'unknown';
export type ReservationStatus = 'unreserved' | 'reserved' | 'consumed' | 'released';

export interface OperationContext {
  scenario_id: string;
  run_id?: string;
  idempotency_key: string;
}

export interface AssetIdentity {
  network: string;
  asset: string;
  token_contract?: string;
  decimals: number;
}

export interface PaymentChallenge {
  scenario_id: string;
  offer_id?: string;
  resource: string;
  protocol: string;
  asset: AssetIdentity;
  amount: string;
  recipient: string;
  nonce: string;
  challenge_hash: string;
  expires_at: number;
  payload: unknown;
}

export interface HttpCredential {
  bound_origin: string;
  header_name: string;
  header_value: string;
}

export interface PaymentCredentialWire {
  protocol: string;
  challenge_hash: string;
  credential_hash: string;
  payload: unknown;
  http?: HttpCredential | undefined;
}

export class PaymentCredential implements PaymentCredentialWire {
  constructor(
    public readonly protocol: string,
    public readonly challenge_hash: string,
    public readonly credential_hash: string,
    public readonly payload: unknown,
    public readonly http?: HttpCredential,
  ) {
    required('credential.protocol', protocol);
    required('credential.challenge_hash', challenge_hash);
    required('credential.credential_hash', credential_hash);
  }

  static fromWire(value: PaymentCredentialWire): PaymentCredential {
    return new PaymentCredential(
      value.protocol,
      value.challenge_hash,
      value.credential_hash,
      value.payload,
      value.http,
    );
  }

  toJSON(): PaymentCredentialWire {
    const value: PaymentCredentialWire = {
      protocol: this.protocol,
      challenge_hash: this.challenge_hash,
      credential_hash: this.credential_hash,
      payload: this.payload,
    };
    if (this.http !== undefined) value.http = this.http;
    return value;
  }

  [inspect.custom](): string {
    return `PaymentCredential { protocol: ${JSON.stringify(this.protocol)}, credential: <redacted> }`;
  }
}

export interface PaymentIntent {
  id: string;
  scenario_id: string;
  run_id: string;
  wallet_id?: string;
  policy_id?: string;
  offer_id?: string;
  direction: PaymentDirection;
  protocol: string;
  asset: AssetIdentity;
  recipient: string;
  amount: string;
  settled_amount: string;
  resource: string;
  challenge_hash?: string;
  idempotency_key: string;
  request_hash: string;
  status: PaymentStatus;
  reservation_status: ReservationStatus;
  reserved_amount: string;
  tx_hash?: string;
  settlement_ref?: string;
  next_reconcile_at?: number;
  created_at: number;
  updated_at: number;
  expires_at?: number;
  settled_at?: number;
}

export interface PaymentReceipt {
  intent_id: string;
  scenario_id: string;
  direction: PaymentDirection;
  protocol: string;
  asset: AssetIdentity;
  recipient: string;
  payer?: string;
  requested_amount: string;
  settled_amount: string;
  status: PaymentStatus;
  tx_hash?: string;
  settlement_ref?: string;
  evidence_hash: string;
  confirmed_at: number;
}

export function required(name: string, value: string): void {
  if (!value.trim()) throw new Error(`${name} cannot be blank`);
}

export function assertIdempotencyKey(value: string): void {
  if (!/^[A-Za-z0-9_.:-]{1,128}$/.test(value)) throw new Error('invalid idempotency key');
}
