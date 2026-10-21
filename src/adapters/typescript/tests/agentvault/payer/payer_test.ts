import assert from 'node:assert/strict';
import test from 'node:test';
import { Challenge, Credential, Receipt } from 'mppx';
import { PayerRequestContext, type AuthorizePaymentResponse } from '../../../src/agentvault/model/payer.js';
import { PaymentCredential } from '../../../src/agentvault/model/base.js';
import type { VaultTransport } from '../../../src/agentvault/transport/transport.js';
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

test('PayerMiddleware forwards native challenge headers to AgentVault', async () => {
  for (const [header, value] of [
    ['payment-required', 'eyJ4NDAyVmVyc2lvbiI6Mn0='],
    ['www-authenticate', 'Payment id="charge", method="tempo"'],
  ] as const) {
    let captured: unknown;
    const transport: VaultTransport = {
      async request<T>(_method, _path, body): Promise<T> {
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
      { status: 402, headers: new Headers({ [header]: value }), body: new TextEncoder().encode('{}') },
      new PayerRequestContext('run-1'),
    ), /vault called/);
    assert.equal((captured as { challenge: { headers: Record<string, string> } })
      .challenge.headers[header], value);
  }
});

test('PayerMiddleware binds MCP challenges to JSON-RPC version and request id', async () => {
  let calls = 0;
  const transport: VaultTransport = {
    async request<T>(): Promise<T> {
      calls += 1;
      throw new Error('Vault must not be called');
    },
  };
  const middleware = new PayerMiddleware(transport, { scenarioId: 'scenario-1' });
  const request = {
    method: 'POST', url: 'https://merchant.test/mcp', origin: 'https://merchant.test',
    headers: new Headers(), followRedirects: false,
    body: new TextEncoder().encode(JSON.stringify({
      jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'marketpulse.realtime' },
    })),
  };
  for (const message of [
    { jsonrpc: '2.0', id: 2, result: { isError: true } },
    { jsonrpc: '1.0', id: 1, result: { isError: true } },
  ]) {
    assert.equal(await middleware.authorizeMcp(
      request, message, new PayerRequestContext('run-1'),
    ), undefined);
  }
  assert.equal(calls, 0);
});

test('PayerMiddleware reports MPP receipts using protocol evidence', async () => {
  let captured: unknown;
  const transport: VaultTransport = {
    async request<T>(_method, path, body): Promise<T> {
      assert.match(path, /outbound-response$/);
      captured = body;
      return { accepted: true, replayed: false, intent: {} } as T;
    },
  };
  const middleware = new PayerMiddleware(transport, { scenarioId: 'scenario-1' });
  const challenge = Challenge.from({
    id: 'challenge-0123456789abcdef',
    realm: 'merchant.example',
    method: 'tempo',
    intent: 'charge',
    request: { amount: '1000', currency: 'USDC' },
    header: 'Payment-Authorization',
  });
  const authorization = Credential.serialize(Credential.from({
    challenge,
    payload: { type: 'hash', hash: '0x1111' },
  }));
  const payment = {
    intent: {
      id: 'intent-1', scenario_id: 'scenario-1', run_id: 'run-1', wallet_id: 'wallet', policy_id: 'policy',
      direction: 'outbound' as const, protocol: 'mpp',
      asset: { network: 'eip155:42431', asset: 'USDC', decimals: 6 }, recipient: '0x1',
      amount: '1000', settled_amount: '0', resource: 'https://merchant.example/tool',
      challenge_hash: 'challenge', request_hash: 'request', status: 'signed' as const,
      reservation_status: 'reserved' as const, reserved_amount: '1000', created_at: 1, updated_at: 1,
    },
    credential: new PaymentCredential('mpp', 'challenge', 'credential', {}, {
      bound_origin: 'https://merchant.example',
      header_name: 'payment-authorization',
      header_value: authorization,
    }),
  };
  const existing = new Headers({ authorization: 'Bearer app-token' });
  assert.equal(middleware.credentialHeader(payment, 'https://merchant.example', existing)[0],
    'payment-authorization');
  await middleware.report(
    new PayerRequestContext('run-1'),
    payment,
    { status: 200, headers: new Headers({
      'payment-receipt': Receipt.serialize(Receipt.from({
        status: 'success',
        method: 'tempo',
        timestamp: '2033-05-18T03:33:20Z',
        reference: '0x1111',
      })),
    }) },
  );
  assert.deepEqual(captured, {
    scenario_id: 'scenario-1',
    run_id: 'run-1',
    protocol: 'mpp',
    evidence: {
      header_name: 'payment-receipt',
      header_value: Receipt.serialize(Receipt.from({
        status: 'success',
        method: 'tempo',
        timestamp: '2033-05-18T03:33:20Z',
        reference: '0x1111',
      })),
    },
  });
});
