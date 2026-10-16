import assert from 'node:assert/strict';
import test from 'node:test';
import { AgentVault } from '../../src/agentvault/agentvault.js';
import type { VaultTransport } from '../../src/agentvault/transport/http.js';

test('AgentVault creates only role-oriented middleware entrypoints', () => {
  const transport: VaultTransport = { async request<T>(): Promise<T> { throw new Error('not called'); } };
  const vault = new AgentVault(transport);
  const payer = vault.payer({ scenarioId: 'scenario-1', walletId: 'wallet-1', protocol: 'x402',
    asset: { network: 'eip155:1', asset: 'USDC', decimals: 6 }, amount: '100', recipient: 'recipient-1' });
  const payee = vault.payee();
  assert.equal(payer.constructor.name, 'PayerMiddleware');
  assert.equal(payee.constructor.name, 'PayeeMiddleware');
  assert.equal('payment' in (vault as unknown as Record<string, unknown>), false);
});
