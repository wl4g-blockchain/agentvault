import assert from 'node:assert/strict';
import test from 'node:test';
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
