import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { encodePaymentSignatureHeader } from '@x402/core/http';
import type { PaymentPayload } from '@x402/core/types';
import { Credential, Mcp } from 'mppx';
import { PayerRequestContext } from '../../../src/agentvault/model/payer.js';
import type { VaultTransport } from '../../../src/agentvault/transport/transport.js';
import { PayerMiddleware } from '../../../src/agentvault/payer/payer.js';

test('FetchPayerMiddleware leaves free responses untouched', async () => {
  const transport: VaultTransport = { async request<T>(): Promise<T> { throw new Error('unexpected Vault call'); } };
  const core = new PayerMiddleware(transport, { scenarioId: 'scenario-1' });
  const middleware = core.standard(async () => new Response('free', { status: 200 }));
  const response = await middleware.request(
    'https://marketpulse.test/mcp/free',
    undefined,
    new PayerRequestContext('run-1'),
  );
  assert.equal(await response.text(), 'free');
});

test('FetchPayerMiddleware closes x402 and MPP MCP metadata flows once', async () => {
  const fixture = JSON.parse(readFileSync(
    new URL('../../../../tests/payment_protocol_mcp.json', import.meta.url),
    'utf8',
  )) as {
    protocols: Record<string, {
      challenge_response: unknown;
      credential_meta: Record<string, unknown>;
      receipt_meta: Record<string, unknown>;
    }>;
  };

  for (const protocol of ['x402', 'mpp'] as const) {
    const contract = fixture.protocols[protocol]!;
    const vaultCalls: Array<{ path: string; body: unknown }> = [];
    const transport: VaultTransport = {
      async request<T>(_method, path, body): Promise<T> {
        vaultCalls.push({ path, body });
        if (path === '/api/v1/payments/authorize') {
          const x402 = contract.credential_meta['x402/payment'];
          const mpp = contract.credential_meta[Mcp.credentialMetaKey];
          return {
            authorization: { outcome: 'authorized', intent_id: 'intent-1' },
            intent: intent(protocol, 'signed'),
            credential: {
              protocol,
              challenge_hash: 'challenge-1',
              credential_hash: 'credential-1',
              payload: x402 ?? mpp,
              http: {
                bound_origin: 'https://merchant.test',
                header_name: protocol === 'x402' ? 'Payment-Signature' : 'Payment-Authorization',
                header_value: protocol === 'x402'
                  ? encodePaymentSignatureHeader(x402 as PaymentPayload)
                  : Credential.serialize(mpp as Credential.Credential),
              },
            },
          } as T;
        }
        if (path === '/api/v1/payments/intent-1/outbound-response') {
          return {
            intent: intent(protocol, 'pending_settlement'),
            accepted: true,
            replayed: false,
          } as T;
        }
        throw new Error(`unexpected AgentVault path: ${path}`);
      },
    };
    const requests: Request[] = [];
    const fetchImpl: typeof globalThis.fetch = async (input) => {
      const request = input instanceof Request ? input : new Request(input);
      requests.push(request);
      assert.equal(request.headers.get('authorization'), 'Bearer application-token');
      assert.equal(request.headers.has('payment-signature'), false);
      assert.equal(request.headers.has('payment-authorization'), false);
      const message = JSON.parse(await request.clone().text()) as {
        id: number;
        params: { _meta?: Record<string, unknown> };
      };
      if (requests.length === 1) {
        assert.equal(message.params._meta, undefined);
        return Response.json(contract.challenge_response);
      }
      const expectedKey = protocol === 'x402' ? 'x402/payment' : Mcp.credentialMetaKey;
      assert.deepEqual(Object.keys(message.params._meta ?? {}), [expectedKey]);
      return Response.json({
        jsonrpc: '2.0',
        id: message.id,
        result: {
          content: [{ type: 'text', text: 'paid' }],
          _meta: contract.receipt_meta,
        },
      });
    };
    const middleware = new PayerMiddleware(transport, { scenarioId: 'scenario' })
      .standard(fetchImpl);
    const response = await middleware.request(
      'https://merchant.test/mcp',
      {
        method: 'POST',
        headers: {
          authorization: 'Bearer application-token',
          'content-type': 'application/json',
        },
        body: JSON.stringify({
          jsonrpc: '2.0',
          id: 1,
          method: 'tools/call',
          params: {
            name: 'marketpulse.realtime',
            arguments: { symbol: 'AVT' },
          },
        }),
      },
      new PayerRequestContext('run-1'),
    );
    assert.equal(response.status, 200);
    assert.equal((await response.json() as { result: { content: Array<{ text: string }> } })
      .result.content[0]?.text, 'paid');
    assert.equal(requests.length, 2);
    assert.equal(vaultCalls.length, 2);
    assert.equal(vaultCalls[0]?.path, '/api/v1/payments/authorize');
    assert.equal(vaultCalls[1]?.path, '/api/v1/payments/intent-1/outbound-response');
    assert.equal((vaultCalls[1]?.body as { protocol: string }).protocol, protocol);
    assert.equal(middleware.payment(response)?.intent.id, 'intent-1');
  }
});

function intent(protocol: string, status: 'signed' | 'pending_settlement') {
  return {
    id: 'intent-1',
    scenario_id: 'scenario',
    run_id: 'run-1',
    wallet_id: 'wallet-1',
    policy_id: 'policy-1',
    direction: 'outbound',
    protocol,
    asset: { network: 'eip155:42431', asset: 'USDC', decimals: 6 },
    recipient: `0x${'11'.repeat(20)}`,
    amount: '1000',
    settled_amount: '0',
    resource: 'https://merchant.test/mcp',
    challenge_hash: 'challenge-1',
    request_hash: 'request-1',
    status,
    reservation_status: 'reserved',
    reserved_amount: '1000',
    created_at: 1,
    updated_at: 2,
  };
}
