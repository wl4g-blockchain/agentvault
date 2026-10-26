import assert from 'node:assert/strict';
import test from 'node:test';
import type { VaultTransport } from '../../../src/agentvault/transport/transport.js';
import { InvalidPaymentError, PayeeMiddleware } from '../../../src/agentvault/payee/payee.js';

test('PayeeMiddleware creates a challenge when no credential is present', async () => {
  const transport: VaultTransport = {
    async request<T>(_method, path): Promise<T> {
      assert.equal(path, '/api/v1/payee/challenges');
      return {
        scenario_id: 'scenario-1', price_id: 'price-1', resource: 'mcp://marketpulse/realtime',
        protocol: 'x402', asset: { network: 'eip155:1', asset: 'USDC', decimals: 6 },
        amount: '100', recipient: 'recipient-1', nonce: 'nonce-1', challenge_hash: 'challenge-1',
        payload: {}, expires_at: 1,
      } as T;
    },
  };
  const outcome = await new PayeeMiddleware(transport).protect({
    scenarioId: 'scenario-1', runId: 'run-1', priceId: 'price-1',
    resource: 'mcp://marketpulse/realtime', origin: 'https://marketpulse.test', protocol: 'x402',
    requestHash: 'request-hash',
  });
  assert.equal(outcome.outcome, 'payment_required');
});

test('PayeeMiddleware rejects invalid payment before settlement', async () => {
  const paths: string[] = [];
  const transport: VaultTransport = {
    async request<T>(_method, path): Promise<T> {
      paths.push(path);
      if (path === '/api/v1/payee/challenges') {
        return {
          scenario_id: 'scenario', price_id: 'price', resource: 'mcp://marketpulse/realtime',
          protocol: 'x402', asset: { network: 'eip155:31337', asset: 'USDC', decimals: 6 },
          amount: '1000', recipient: '0x1', nonce: 'nonce', challenge_hash: 'challenge',
          expires_at: 1, payload: { payment_required: {} },
        } as T;
      }
      if (path === '/api/v1/payee/verify') {
        return {
          intent: {
            id: 'intent', scenario_id: 'scenario', run_id: 'run', price_id: 'price',
            direction: 'inbound', protocol: 'x402',
            asset: { network: 'eip155:31337', asset: 'USDC', decimals: 6 },
            recipient: '0x1', amount: '1000', settled_amount: '0',
            resource: 'mcp://marketpulse/realtime', challenge_hash: 'challenge',
            request_hash: 'request-hash',
            status: 'pending_settlement', reservation_status: 'reserved', reserved_amount: '1000',
            created_at: 1, updated_at: 1,
          },
          valid: false,
        } as T;
      }
      throw new Error(`unexpected Vault call after invalid verification: ${path}`);
    },
  };
  await assert.rejects(
    () => new PayeeMiddleware(transport).protect({
      scenarioId: 'scenario', runId: 'run', priceId: 'price',
      resource: 'mcp://marketpulse/realtime', origin: 'https://marketpulse.example',
      protocol: 'x402', requestHash: 'request-hash', credentialPayload: { signature: 'present' },
    }),
    InvalidPaymentError,
  );
  assert.deepEqual(paths, ['/api/v1/payee/challenges', '/api/v1/payee/verify']);
});
