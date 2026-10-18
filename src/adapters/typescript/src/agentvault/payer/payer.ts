import type { PayerConfig } from '../config/config.js';
import { PaymentCredential, type OperationContext, type PaymentIntent, type PaymentReceipt } from '../model/base.js';
import {
  type AuthorizedPayment,
  type AuthorizePaymentRequest,
  type AuthorizePaymentResponse,
  type BalanceQuery,
  type PaymentBalance,
  type PayerHttpRequest,
  type PayerHttpResponse,
  PayerRequestContext,
  type PaymentRequired,
  type ReportOutboundResponseRequest,
  type ReportOutboundResponseResponse,
  validateBalanceQuery,
} from '../model/payer.js';
import type { VaultTransport } from '../transport/transport.js';
import { FetchPayerMiddleware } from './standard.js';

const SAFE_VALUE = /^[^\r\n]+$/;
const SAFE_HEADER = /^[!#$%&'*+\-.^_`|~A-Za-z0-9]+$/;
const FORBIDDEN_CREDENTIAL_HEADERS = new Set([
  'host', 'content-length', 'authorization', 'connection', 'transfer-encoding',
  'proxy-authorization', 'proxy-authenticate', 'upgrade', 'te', 'trailer', 'accept', 'content-type',
]);

export class PayerMiddleware {
  readonly config: Required<PayerConfig>;

  constructor(private readonly transport: VaultTransport, config: PayerConfig) {
    if (!transport) throw new Error('AgentVault transport is not configured');
    this.config = {
      ...config,
      approvalTimeoutMs: config.approvalTimeoutMs ?? 120_000,
      approvalPollIntervalMs: config.approvalPollIntervalMs ?? 1_000,
      deliveryRetries: config.deliveryRetries ?? 0,
      maxChallengeBytes: config.maxChallengeBytes ?? 256 * 1024,
    };
    if (!config.scenarioId.trim()) throw new Error('scenarioId cannot be blank');
    if (this.config.approvalTimeoutMs <= 0 || this.config.approvalPollIntervalMs <= 0 ||
        this.config.approvalPollIntervalMs > this.config.approvalTimeoutMs ||
        !Number.isInteger(this.config.deliveryRetries) || this.config.deliveryRetries < 0 ||
        this.config.deliveryRetries > 3 || this.config.maxChallengeBytes <= 0) {
      throw new Error('invalid payer middleware limits');
    }
  }

  standard(fetchImpl: typeof globalThis.fetch = globalThis.fetch): FetchPayerMiddleware {
    return new FetchPayerMiddleware(this, fetchImpl);
  }

  async authorize(
    request: PayerHttpRequest,
    response: PayerHttpResponse,
    operation: PayerRequestContext,
  ): Promise<AuthorizedPayment | undefined> {
    if (response.status !== 402) return undefined;
    const body = response.body ?? new Uint8Array();
    if (body.length > this.config.maxChallengeBytes) {
      throw new Error('payment challenge exceeds configured limit');
    }
    const target = safeHttpUrl(request.url);
    if (request.followRedirects || target.origin.toLowerCase() !== request.origin.toLowerCase()) {
      throw new Error('paid request must use a canonical origin with redirects disabled');
    }
    const challenge: PaymentRequired = {
      origin: request.origin,
      resource: request.url,
      content_type: response.headers.get('content-type') ?? 'application/json',
      headers: Object.fromEntries(response.headers.entries()),
      body: new TextDecoder('utf-8', { fatal: true }).decode(body),
    };
    const protocolHint = response.headers.get('payment-protocol');
    const expiresAt = response.headers.get('payment-expires-at');
    if (protocolHint) challenge.protocol_hint = protocolHint;
    if (expiresAt) challenge.expires_at = expiresAt;
    const authorization = await this.authorizeUntilTerminal({
      scenario_id: this.config.scenarioId,
      run_id: operation.runId,
      challenge,
    });
    if (authorization.authorization.outcome === 'denied') {
      throw new Error(`AgentVault denied the payment: ${authorization.authorization.reason ?? 'policy denied'}`);
    }
    if (authorization.authorization.outcome !== 'authorized' || !authorization.credential) {
      throw new Error('approval wait returned a non-terminal payment result');
    }
    const credential = PaymentCredential.fromWire(authorization.credential);
    const intent = authorization.intent;
    if (!intent.id || intent.scenario_id !== this.config.scenarioId || intent.run_id !== operation.runId ||
        !intent.wallet_id || !intent.policy_id || intent.direction !== 'outbound' ||
        (protocolHint !== null && intent.protocol !== protocolHint) || intent.protocol !== credential.protocol ||
        intent.resource !== request.url ||
        intent.challenge_hash !== credential.challenge_hash) {
      throw new Error('credential, intent, and resource are not bound to the same payment');
    }
    return { intent, credential };
  }

  credentialHeader(payment: AuthorizedPayment, origin: string, existing: Headers): [string, string] {
    const delivery = payment.credential.http;
    if (!delivery || delivery.bound_origin.toLowerCase() !== origin.toLowerCase() ||
        !SAFE_HEADER.test(delivery.header_name) || !SAFE_VALUE.test(delivery.header_value) ||
        FORBIDDEN_CREDENTIAL_HEADERS.has(delivery.header_name.toLowerCase()) ||
        existing.has(delivery.header_name)) {
      throw new Error('unsafe or missing credential HTTP binding');
    }
    return [delivery.header_name, delivery.header_value];
  }

  async report(
    operation: PayerRequestContext,
    payment: AuthorizedPayment,
    response: PayerHttpResponse,
  ): Promise<void> {
    if (payment.credential.protocol !== 'x402') return;
    const header = response.headers.get('payment-response');
    if (header) {
      await this.reportOutboundResponse(
        operation.operationContext(this.config.scenarioId),
        payment.intent.id,
        header,
      );
    } else if (response.status >= 200 && response.status < 300) {
      throw new Error('successful x402 response omitted PAYMENT-RESPONSE; payment remains reserved');
    }
  }

  private async authorizeUntilTerminal(request: AuthorizePaymentRequest): Promise<AuthorizePaymentResponse> {
    const deadline = Date.now() + this.config.approvalTimeoutMs;
    let expectedIntent: string | undefined;
    for (;;) {
      const response = await this.transport.request<AuthorizePaymentResponse>(
        'POST',
        '/api/v1/payments/authorize',
        request,
      );
      const { authorization, intent } = response;
      if (!authorization || !['authorized', 'requires_approval', 'denied'].includes(authorization.outcome) ||
          !authorization.intent_id || intent?.id !== authorization.intent_id) {
        throw new Error('invalid AgentVault authorization response');
      }
      if (expectedIntent === undefined) expectedIntent = authorization.intent_id;
      else if (authorization.intent_id !== expectedIntent) {
        throw new Error('replayed run changed payment intent');
      }
      if (authorization.outcome !== 'requires_approval') return response;
      const remaining = deadline - Date.now();
      if (remaining <= 0) throw new Error('payment approval did not complete before the total timeout');
      await new Promise((resolve) => setTimeout(resolve, Math.min(this.config.approvalPollIntervalMs, remaining)));
    }
  }

  getPayment(scenarioId: string, intentId: string): Promise<PaymentIntent> {
    if (!scenarioId.trim() || !intentId.trim()) throw new Error('scenarioId and intentId cannot be blank');
    return this.transport.request(
      'GET',
      `/api/v1/payments/${encodeURIComponent(intentId)}?scenario_id=${encodeURIComponent(scenarioId)}`,
    );
  }

  getReceipt(scenarioId: string, intentId: string): Promise<PaymentReceipt> {
    if (!scenarioId.trim() || !intentId.trim()) throw new Error('scenarioId and intentId cannot be blank');
    return this.transport.request(
      'GET',
      `/api/v1/payments/${encodeURIComponent(intentId)}/receipt?scenario_id=${encodeURIComponent(scenarioId)}`,
    );
  }

  getBalance(query: BalanceQuery): Promise<PaymentBalance> {
    validateBalanceQuery(query);
    const params = new URLSearchParams({
      scenario_id: query.scenarioId,
      wallet_id: query.walletId,
      network: query.asset.network,
      asset: query.asset.asset,
      decimals: String(query.asset.decimals),
    });
    if (query.asset.token_contract) params.set('token_contract', query.asset.token_contract);
    return this.transport.request('GET', `/api/v1/payments/balance?${params.toString()}`);
  }

  private reportOutboundResponse(
    context: OperationContext,
    intentId: string,
    paymentResponse: string,
  ): Promise<ReportOutboundResponseResponse> {
    if (!context.run_id.trim()) throw new Error('run_id is required');
    if (!intentId.trim()) throw new Error('intent_id cannot be blank');
    if (!paymentResponse || paymentResponse.length > 16 * 1024 || !/^[\x21-\x7e]+$/.test(paymentResponse)) {
      throw new Error('payment_response must be a bounded official x402 header value');
    }
    const request: ReportOutboundResponseRequest = { ...context, payment_response: paymentResponse };
    return this.transport.request(
      'POST',
      `/api/v1/payments/${encodeURIComponent(intentId)}/outbound-response`,
      request,
    );
  }
}

export function safeHttpUrl(value: string): URL {
  const target = new URL(value);
  if (!['http:', 'https:'].includes(target.protocol) || target.username || target.password) {
    throw new Error('paid request URL must be absolute HTTP(S) without userinfo');
  }
  return target;
}
