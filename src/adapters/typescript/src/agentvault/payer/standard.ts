import { readBounded } from '../transport/transport.js';
import type { PayerRequestContext } from '../model/payer.js';
import {
  PayerMiddleware,
  safeHttpUrl,
} from './payer.js';
import type { AuthorizedPayment, PayerHttpResponse } from '../model/payer.js';

/** Native Fetch payer adapter with transparent 402 handling and bounded retries. */
export class FetchPayerMiddleware {
  private readonly payments = new WeakMap<Response, AuthorizedPayment>();

  constructor(
    private readonly core: PayerMiddleware,
    private readonly fetchImpl: typeof globalThis.fetch = globalThis.fetch,
  ) {}

  payment(response: Response): AuthorizedPayment | undefined {
    return this.payments.get(response);
  }

  async request(
    input: URL | RequestInfo,
    init?: RequestInit,
    operation?: PayerRequestContext,
  ): Promise<Response> {
    const source = new Request(input, init);
    if (!operation) return this.fetchImpl(source);
    const request = new Request(source, { redirect: 'manual' });
    const target = safeHttpUrl(request.url);
    const replay = request.clone();
    const first = await this.fetchImpl(request);
    if (first.status !== 402) return first;
    sdkEvent('agentvault.sdk.payer.x402_detected', { runId: operation.runId });
    const challengeBody = await readBounded(first.clone(), this.core.config.maxChallengeBytes);
    sdkEvent('agentvault.sdk.payer.signing_requested', { runId: operation.runId });
    const authorized = await this.core.authorize(
      {
        method: replay.method, url: target.href, origin: target.origin,
        headers: replay.headers, followRedirects: false,
      },
      { status: first.status, headers: first.headers, body: challengeBody },
      operation,
    );
    if (!authorized) return first;
    sdkEvent('agentvault.sdk.payer.signed', {
      runId: operation.runId, intentId: authorized.intent.id,
    });
    const [headerName, headerValue] = this.core.credentialHeader(authorized, target.origin, replay.headers);
    const headers = new Headers(replay.headers);
    headers.set(headerName, headerValue);
    for (let attempt = 0; attempt <= this.core.config.deliveryRetries; attempt += 1) {
      sdkEvent('agentvault.sdk.payer.retrying', {
        runId: operation.runId, intentId: authorized.intent.id, attemptNo: attempt + 1,
      });
      const paid = new Request(replay.clone(), { headers, redirect: 'manual' });
      const response = await this.fetchImpl(paid);
      const responseView: PayerHttpResponse = { status: response.status, headers: response.headers };
      await this.core.report(operation, authorized, responseView);
      if (response.status !== 503 || attempt === this.core.config.deliveryRetries) {
        this.payments.set(response, authorized);
        return response;
      }
    }
    throw new Error('payer retry loop ended unexpectedly');
  }
}

function sdkEvent(event: string, fields: Record<string, unknown>): void {
  console.info(JSON.stringify({ event, ...fields }));
}
