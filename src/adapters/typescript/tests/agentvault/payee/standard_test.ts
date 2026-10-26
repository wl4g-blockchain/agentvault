import assert from 'node:assert/strict';
import test from 'node:test';
import type { VaultTransport } from '../../../src/agentvault/transport/transport.js';
import { PayeeMiddleware } from '../../../src/agentvault/payee/payee.js';

test('StandardPayeeMiddleware delegates to the shared payee state machine', async () => {
  const transport: VaultTransport = {
    async request<T>(): Promise<T> {
      return {
        scenario_id: 'scenario-1', price_id: 'price-1', resource: 'mcp://marketpulse/realtime',
        protocol: 'x402', asset: { network: 'eip155:1', asset: 'USDC', decimals: 6 },
        amount: '100', recipient: 'recipient-1', nonce: 'nonce-1', challenge_hash: 'challenge-1',
        payload: {}, expires_at: 1,
      } as T;
    },
  };
  const outcome = await new PayeeMiddleware(transport).standard().handle({
    scenarioId: 'scenario-1', runId: 'run-1', priceId: 'price-1',
    resource: 'mcp://marketpulse/realtime', origin: 'https://marketpulse.test', protocol: 'x402',
    requestHash: 'request-hash',
  });
  assert.equal(outcome.outcome, 'payment_required');
});
