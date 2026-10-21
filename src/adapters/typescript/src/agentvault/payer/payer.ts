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
  type ProtocolEvidence,
  type ReportOutboundResponseRequest,
  type ReportOutboundResponseResponse,
  validateBalanceQuery,
} from '../model/payer.js';
import type { VaultTransport } from '../transport/transport.js';
import {
  PaymentProtocolFactory,
  type McpMetadata,
  type ProtocolRegistry,
} from '../protocol/adapter.js';
import { FetchPayerMiddleware } from './standard.js';

const SAFE_VALUE = /^[^\r\n]+$/;
const SAFE_HEADER = /^[!#$%&'*+\-.^_`|~A-Za-z0-9]+$/;
const FORBIDDEN_CREDENTIAL_HEADERS = new Set([
  'host', 'content-length', 'connection', 'transfer-encoding',
  'proxy-authorization', 'proxy-authenticate', 'upgrade', 'te', 'trailer', 'accept', 'content-type',
]);

export class PayerMiddleware {
  readonly config: Required<PayerConfig>;
  private readonly protocols: ProtocolRegistry;

  constructor(private readonly transport: VaultTransport, config: PayerConfig) {
    if (!transport) throw new Error('AgentVault transport is not configured');
    this.config = {
      ...config,
      approvalTimeoutMs: config.approvalTimeoutMs ?? 120_000,
      approvalPollIntervalMs: config.approvalPollIntervalMs ?? 1_000,
      deliveryRetries: config.deliveryRetries ?? 0,
      maxChallengeBytes: config.maxChallengeBytes ?? 256 * 1024,
      protocols: config.protocols ?? ['x402', 'mpp'],
    };
    if (!config.scenarioId.trim()) throw new Error('scenarioId cannot be blank');
    if (this.config.approvalTimeoutMs <= 0 || this.config.approvalPollIntervalMs <= 0 ||
        this.config.approvalPollIntervalMs > this.config.approvalTimeoutMs ||
        !Number.isInteger(this.config.deliveryRetries) || this.config.deliveryRetries < 0 ||
        this.config.deliveryRetries > 3 || this.config.maxChallengeBytes <= 0) {
      throw new Error('invalid payer middleware limits');
    }
    this.protocols = new PaymentProtocolFactory().create(this.config.protocols);
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
    if (protocolHint) this.protocols.get(protocolHint);
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
    this.protocols.get(intent.protocol);
    if (!intent.id || intent.scenario_id !== this.config.scenarioId || intent.run_id !== operation.runId ||
        !intent.wallet_id || !intent.policy_id || intent.direction !== 'outbound' ||
        (protocolHint !== null && intent.protocol !== protocolHint) || intent.protocol !== credential.protocol ||
        intent.resource !== request.url ||
        intent.challenge_hash !== credential.challenge_hash) {
      throw new Error('credential, intent, and resource are not bound to the same payment');
    }
    return { intent, credential };
  }

  async authorizeMcp(
    request: PayerHttpRequest,
    message: unknown,
    operation: PayerRequestContext,
    responseHeaders: Headers = new Headers(),
  ): Promise<AuthorizedPayment | undefined> {
    const requestId = this.mcpCallId(request.body);
    if (requestId === undefined || !this.boundMcpResponse(message, requestId, false)) {
      return undefined;
    }
    const selected = this.protocols.mcpChallenge(message, this.config.maxChallengeBytes);
    if (!selected) return undefined;
    if (responseHeaders.has('payment-required')) {
      throw new Error('MCP payment challenge must not also declare PAYMENT-REQUIRED');
    }
    const advertised = responseHeaders.get('payment-protocol');
    if (advertised && advertised.toLowerCase() !== selected.adapter.name.toLowerCase()) {
      throw new Error('MCP payment challenge protocol metadata conflicts with its JSON-RPC payload');
    }
    const headers = new Headers(selected.challenge.headers);
    headers.set('payment-protocol', selected.adapter.name);
    const expiresAt = responseHeaders.get('payment-expires-at');
    if (expiresAt) headers.set('payment-expires-at', expiresAt);
    return this.authorize(
      request,
      {
        status: 402,
        headers,
        body: selected.challenge.body ?? new Uint8Array(),
      },
      operation,
    );
  }

  mcpCredential(payment: AuthorizedPayment, origin: string): McpMetadata {
    if (payment.intent.protocol !== payment.credential.protocol) {
      throw new Error('outbound payment protocol binding is invalid');
    }
    const adapter = this.protocols.mcp(payment.credential.protocol);
    const delivery = adapter.credential(payment.credential);
    if (delivery.bound_origin.toLowerCase() !== origin.toLowerCase()) {
      throw new Error('unsafe or missing MCP credential origin binding');
    }
    return adapter.mcpCredential(payment.credential);
  }

  mcpRequestMetadataKeys(): readonly string[] {
    return this.protocols.mcpRequestMetadataKeys();
  }

  mcpRootRequestMetadataKeys(): readonly string[] {
    return this.protocols.mcpRootRequestMetadataKeys();
  }

  credentialHeader(payment: AuthorizedPayment, origin: string, existing: Headers): [string, string] {
    const delivery = this.protocols.get(payment.credential.protocol).credential(payment.credential);
    if (delivery.bound_origin.toLowerCase() !== origin.toLowerCase() ||
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
    if (payment.intent.protocol !== payment.credential.protocol) {
      throw new Error('outbound payment protocol binding is invalid');
    }
    const adapter = this.protocols.get(payment.credential.protocol);
    const [protocol, evidence] = this.protocols.merchantEvidence(response.headers);
    if (evidence && protocol !== adapter.name) {
      throw new Error('HTTP settlement receipt protocol does not match the paid credential');
    }
    if (evidence) {
      await this.reportOutboundResponse(
        operation.operationContext(this.config.scenarioId),
        payment.intent.id,
        adapter.name,
        { header_name: evidence.headerName, header_value: evidence.headerValue },
      );
    } else if (response.status >= 200 && response.status < 300) {
      throw new Error(`successful ${adapter.name} response omitted settlement evidence; payment remains reserved`);
    }
  }

  async reportMcp(
    operation: PayerRequestContext,
    payment: AuthorizedPayment,
    meta: unknown,
  ): Promise<void> {
    if (payment.intent.protocol !== payment.credential.protocol) {
      throw new Error('outbound payment protocol binding is invalid');
    }
    const [protocol, evidence] = this.protocols.mcpMerchantEvidence(
      meta,
      this.config.maxChallengeBytes,
    );
    if (!evidence || protocol !== payment.intent.protocol) {
      throw new Error(
        `successful ${payment.intent.protocol} MCP response omitted matching settlement evidence; ` +
        'payment remains reserved',
      );
    }
    await this.reportOutboundResponse(
      operation.operationContext(this.config.scenarioId),
      payment.intent.id,
      protocol,
      { header_name: evidence.headerName, header_value: evidence.headerValue },
    );
  }

  async deliver<T>(
    operation: PayerRequestContext,
    payment: AuthorizedPayment,
    delivery: {
      send(attemptNo: number): Promise<T>;
      view(response: T): PayerHttpResponse;
      discard(response: T): Promise<void>;
      report?(response: T): Promise<void>;
    },
  ): Promise<T> {
    for (let attempt = 0; attempt <= this.config.deliveryRetries; attempt += 1) {
      console.info(JSON.stringify({
        event: 'agentvault.sdk.payer.retrying',
        runId: operation.runId,
        intentId: payment.intent.id,
        attemptNo: attempt + 1,
      }));
      const response = await delivery.send(attempt + 1);
      const responseView = delivery.view(response);
      try {
        if (delivery.report) await delivery.report(response);
        else await this.report(operation, payment, responseView);
      } catch (error) {
        await delivery.discard(response);
        throw error;
      }
      if (responseView.status !== 503 || attempt === this.config.deliveryRetries) return response;
      await delivery.discard(response);
    }
    throw new Error('payer retry loop ended unexpectedly');
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
    protocol: string,
    evidence: ProtocolEvidence,
  ): Promise<ReportOutboundResponseResponse> {
    if (!context.run_id.trim()) throw new Error('run_id is required');
    if (!intentId.trim()) throw new Error('intent_id cannot be blank');
    if (!protocol.trim() || !SAFE_HEADER.test(evidence.header_name) || !evidence.header_value ||
        evidence.header_value.length > 64 * 1024 || !/^[\x21-\x7e]+$/.test(evidence.header_value)) {
      throw new Error('protocol merchant evidence is invalid');
    }
    const request: ReportOutboundResponseRequest = { ...context, protocol, evidence };
    return this.transport.request(
      'POST',
      `/api/v1/payments/${encodeURIComponent(intentId)}/outbound-response`,
      request,
    );
  }

  private mcpCallId(body: Uint8Array | undefined): string | number | undefined {
    if (!body) return undefined;
    let message: unknown;
    try {
      message = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(body)) as unknown;
    } catch {
      return undefined;
    }
    if (!this.isRecord(message) || message.jsonrpc !== '2.0' ||
        message.method !== 'tools/call' || !this.isRecord(message.params) ||
        (typeof message.id !== 'string' && typeof message.id !== 'number') ||
        (typeof message.id === 'number' && !Number.isFinite(message.id))) return undefined;
    return message.id;
  }

  private boundMcpResponse(message: unknown, expectedId: string | number, requireResult: boolean): boolean {
    return this.isRecord(message) && message.jsonrpc === '2.0' && message.id === expectedId &&
      (!requireResult || this.isRecord(message.result));
  }

  private isRecord(value: unknown): value is Record<string, unknown> {
    return typeof value === 'object' && value !== null && !Array.isArray(value);
  }
}

export function safeHttpUrl(value: string): URL {
  const target = new URL(value);
  if (!['http:', 'https:'].includes(target.protocol) || target.username || target.password) {
    throw new Error('paid request URL must be absolute HTTP(S) without userinfo');
  }
  return target;
}
