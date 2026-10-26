import type { PayerMiddleware } from './payer.js';
import type { AuthorizedPayment, PayerHttpRequest, PayerRequestContext } from '../model/payer.js';
import { A2ACall } from '../model/a2a.js';
import type { ProtocolRegistry } from '../protocol/adapter.js';

/** Carrier projection around the existing payer state machine. */
export class A2APayerMiddleware {
  constructor(private readonly core: PayerMiddleware, private readonly protocols: ProtocolRegistry) {}

  extensions(existing = ''): string { return this.protocols.a2aExtensions(existing); }

  async authorize(request: PayerHttpRequest, raw: unknown, operation: PayerRequestContext, headers: Headers): Promise<AuthorizedPayment | undefined> {
    const call = A2ACall.parse(request.body ?? new Uint8Array(), this.core.config.maxChallengeBytes);
    if (!call) return undefined;
    const message = A2ACall.response(raw, call.envelope.id);
    const selected = this.protocols.a2aChallenge(message, this.core.config.maxChallengeBytes);
    if (!selected) return undefined;
    const [adapter, challenge] = selected;
    if (headers.has('payment-required') || headers.has('www-authenticate')) throw new Error('A2A challenge conflicts with HTTP authentication headers');
    if (adapter.a2aExtension() && !headers.get('x-a2a-extensions')?.split(',').map((value) => value.trim()).includes(adapter.a2aExtension())) {
      throw new Error('A2A extension activation was not acknowledged');
    }
    const wire = new Headers(challenge.headers);
    wire.set('payment-protocol', adapter.name);
    wire.set('content-type', challenge.contentType ?? 'application/json');
    const expiry = headers.get('payment-expires-at');
    if (expiry) wire.set('payment-expires-at', expiry);
    return this.core.authorize(request, {status: 402, headers: wire, ...(challenge.body === undefined ? {} : {body: challenge.body})}, operation);
  }

  credential(payment: AuthorizedPayment, call: A2ACall, challenge: unknown, origin: string): string {
    const adapter = this.protocols.a2a(payment.credential.protocol);
    if (payment.intent.protocol !== adapter.name || adapter.credential(payment.credential).bound_origin !== origin) throw new Error('A2A credential protocol/origin mismatch');
    if (this.protocols.a2aCredential(call.wire, this.core.config.maxChallengeBytes)[1] !== undefined) throw new Error('A2A request already contains payment credentials');
    const response = A2ACall.response(challenge, call.envelope.id);
    if (response.result !== undefined) call.bindTask(A2ACall.task(response));
    adapter.a2aApplyCredential(call.wire, payment.credential);
    return call.body();
  }

  async report(operation: PayerRequestContext, payment: AuthorizedPayment, call: A2ACall, raw: unknown, headers: Headers): Promise<void> {
    const message = A2ACall.response(raw, call.envelope.id);
    if (message.result !== undefined) call.bindTask(A2ACall.task(message));
    const [name, evidence] = this.protocols.a2aEvidence(message, this.core.config.maxChallengeBytes);
    if (!evidence || name !== payment.credential.protocol) throw new Error('A2A response has no matching receipt; reservation remains pending');
    const existing = headers.get(evidence.headerName);
    if (existing !== null && existing !== evidence.headerValue) throw new Error('A2A receipt conflicts with HTTP receipt');
    await this.core.report(operation, payment, {status: 200, headers: new Headers({[evidence.headerName]: evidence.headerValue})});
  }
}
