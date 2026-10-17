import assert from 'node:assert/strict';
import test from 'node:test';
import { operationContext, validatePayeeOperation } from '../../../src/agentvault/model/payee.js';

test('payee phases reuse one run identity', () => {
  const operation = {
    scenarioId: 'scenario', runId: 'run', offerId: 'offer',
    resource: 'mcp://marketpulse/realtime', origin: 'https://marketpulse.test', protocol: 'x402', requestHash: 'hash',
  };
  assert.doesNotThrow(() => validatePayeeOperation(operation));
  assert.equal(operationContext(operation).run_id, 'run');
});
