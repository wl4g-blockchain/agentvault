import assert from 'node:assert/strict';
import test from 'node:test';
import { PayerRequestContext, type AuthorizePaymentResponse } from '../../../src/agentvault/model/payer.js';
import type { VaultTransport } from '../../../src/agentvault/transport/http.js';
import { PayerMiddleware } from '../../../src/agentvault/payer/payer.js';

test('PayerMiddleware retries the same immutable authorization while approval is pending', async () => {
  const calls: Array<{ body: unknown; key?: string }> = [];
  const responses = [
    { authorization: { outcome: 'requires_approval', intent_id: 'intent-1', approval_id: 'approval-1' },
      intent: { id: 'intent-1' } },
    {
      authorization: { outcome: 'authorized', intent_id: 'intent-1' },
      intent: {
        id: 'intent-1', scenario_id: 'scenario-1', run_id: 'run-1', wallet_id: 'wallet-1',
        direction: 'outbound', protocol: 'x402',
        asset: { network: 'eip155:1', asset: 'USDC', decimals: 6 }, amount: '100',
        recipient: '0x0000000000000000000000000000000000000002',
        resource: 'https://merchant.test/mcp', challenge_hash: 'challenge-1',
        idempotency_key: 'run-1:paid',
      },
      credential: {
        protocol: 'x402', challenge_hash: 'challenge-1', credential_hash: 'credential-1', payload: {},
      },
    },
  ] as unknown as AuthorizePaymentResponse[];
  const transport: VaultTransport = {
    async request<T>(_method, _path, body, key): Promise<T> {
      calls.push({ body, ...(key === undefined ? {} : { key }) });
      return responses.shift() as T;
    },
  };
  const middleware = new PayerMiddleware(transport, {
    scenarioId: 'scenario-1', walletId: 'wallet-1', protocol: 'x402',
    asset: { network: 'eip155:1', asset: 'USDC', decimals: 6 }, amount: '100',
    recipient: '0x0000000000000000000000000000000000000002', approvalPollIntervalMs: 1,
  });
  const response = await middleware.authorize(
    {
      method: 'POST', url: 'https://merchant.test/mcp', origin: 'https://merchant.test',
      headers: new Headers(), followRedirects: false,
    },
    { status: 402, headers: new Headers(), body: new TextEncoder().encode('{}') },
    PayerRequestContext.forOperation('run-1', 'paid'),
  );
  assert.equal(response?.intent.id, 'intent-1');
  assert.equal(calls.length, 2);
  assert.strictEqual(calls[0]?.body, calls[1]?.body);
  assert.equal(calls[0]?.key, calls[1]?.key);
});

test('PayerMiddleware rejects a protocol hint mismatch before calling AgentVault', async () => {
  let called = false;
  const transport: VaultTransport = {
    async request<T>(): Promise<T> {
      called = true;
      throw new Error('must not be called');
    },
  };
  const middleware = new PayerMiddleware(transport, {
    scenarioId: 'scenario-1', walletId: 'wallet-1', protocol: 'x402',
    asset: { network: 'eip155:1', asset: 'USDC', decimals: 6 }, amount: '100',
    recipient: '0x0000000000000000000000000000000000000002',
  });
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
    PayerRequestContext.forOperation('run-1', 'paid'),
  ), /protocol hint/);
  assert.equal(called, false);
});
