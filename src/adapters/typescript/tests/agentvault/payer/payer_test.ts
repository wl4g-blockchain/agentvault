import assert from 'node:assert/strict';
import test from 'node:test';
import { PayerRequestContext, type AuthorizePaymentResponse } from '../../../src/agentvault/model/payer.js';
import type { VaultTransport } from '../../../src/agentvault/transport/http.js';
import { PayerMiddleware } from '../../../src/agentvault/payer/payer.js';

test('PayerMiddleware retries the same immutable authorization while approval is pending', async () => {
  const calls: Array<{ body: unknown }> = [];
  const responses = [
    { authorization: { outcome: 'requires_approval', intent_id: 'intent-1', approval_id: 'approval-1' },
      intent: { id: 'intent-1' } },
    {
      authorization: { outcome: 'authorized', intent_id: 'intent-1' },
      intent: {
        id: 'intent-1', scenario_id: 'scenario-1', run_id: 'run-1', wallet_id: 'wallet-1', policy_id: 'policy-1',
        direction: 'outbound', protocol: 'x402',
        asset: { network: 'eip155:1', asset: 'USDC', decimals: 6 }, amount: '100',
        recipient: '0x0000000000000000000000000000000000000002',
        resource: 'https://merchant.test/mcp', challenge_hash: 'challenge-1',
      },
      credential: {
        protocol: 'x402', challenge_hash: 'challenge-1', credential_hash: 'credential-1', payload: {},
      },
    },
  ] as unknown as AuthorizePaymentResponse[];
  const transport: VaultTransport = {
    async request<T>(_method, _path, body): Promise<T> {
      calls.push({ body });
      return responses.shift() as T;
    },
  };
  const middleware = new PayerMiddleware(transport, { scenarioId: 'scenario-1', approvalPollIntervalMs: 1 });
  const response = await middleware.authorize(
    {
      method: 'POST', url: 'https://merchant.test/mcp', origin: 'https://merchant.test',
      headers: new Headers(), followRedirects: false,
    },
    { status: 402, headers: new Headers(), body: new TextEncoder().encode('{}') },
    new PayerRequestContext('run-1'),
  );
  assert.equal(response?.intent.id, 'intent-1');
  assert.equal(calls.length, 2);
  assert.strictEqual(calls[0]?.body, calls[1]?.body);
  const wire = calls[0]?.body as Record<string, unknown>;
  assert.equal(wire.run_id, 'run-1');
  assert.deepEqual(Object.keys(wire).sort(), ['challenge', 'run_id', 'scenario_id']);
});

test('PayerMiddleware forwards captured protocol hints without local projections', async () => {
  let called = false;
  let captured: unknown;
  const transport: VaultTransport = {
    async request<T>(_method, _path, body): Promise<T> {
      called = true;
      captured = body;
      throw new Error('vault called');
    },
  };
  const middleware = new PayerMiddleware(transport, { scenarioId: 'scenario-1' });
  await assert.rejects(() => middleware.authorize(
    {
      method: 'POST', url: 'https://merchant.test/mcp', origin: 'https://merchant.test',
      headers: new Headers(), followRedirects: false,
    },
    {
      status: 402,
      headers: new Headers({ 'payment-protocol': 'mpp' }),
      body: new TextEncoder().encode('{}'),
    },
    new PayerRequestContext('run-1'),
  ), /vault called/);
  assert.equal(called, true);
  assert.equal((captured as { challenge: { protocol_hint: string } }).challenge.protocol_hint, 'mpp');
});
