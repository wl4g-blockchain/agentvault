import assert from 'node:assert/strict';
import test from 'node:test';
import type { VaultTransport } from '../../../src/agentvault/transport/transport.js';
import { PayeeMiddleware } from '../../../src/agentvault/payee/payee.js';
import { mcpToolRoute } from '../../../src/agentvault/payee/express.js';

test('ExpressPayeeMiddleware exposes raw-body and protection handlers', () => {
  const transport: VaultTransport = { async request<T>(): Promise<T> { throw new Error('not called'); } };
  const middleware = new PayeeMiddleware(transport).express([
    mcpToolRoute('/mcp', 'marketpulse.realtime_news', 'scenario-1', 'offer-1',
      'mcp://marketpulse/realtime', 'https://marketpulse.test'),
  ]);
  assert.equal(middleware.handlers().length, 2);
});

test('ExpressPayeeMiddleware bypasses a free tool on the shared MCP route', async () => {
  const transport: VaultTransport = { async request<T>(): Promise<T> { throw new Error('Vault must not be called'); } };
  const middleware = new PayeeMiddleware(transport).express([
    mcpToolRoute('/mcp', 'marketpulse.realtime_news', 'scenario-1', 'offer-1',
      'mcp://marketpulse/realtime', 'https://marketpulse.test'),
  ]);
  let passed = false;
  const request = {
    method: 'POST', path: '/mcp', headers: {},
    body: Buffer.from(JSON.stringify({ method: 'tools/call', params: { name: 'marketpulse.yesterday_news' } })),
  };
  await middleware.handler()(request as never, {} as never, () => { passed = true; });
  assert.equal(passed, true);
});
