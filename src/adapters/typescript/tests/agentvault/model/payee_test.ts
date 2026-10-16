import assert from 'node:assert/strict';
import test from 'node:test';
import { operationContext, validatePayeeOperation } from '../../../src/agentvault/model/payee.js';

test('payee phase keys are bound to one execution', () => {
  const operation = {
    scenarioId: 'scenario', runId: 'run', executionKey: 'run.realtime', offerId: 'offer',
    resource: 'mcp://marketpulse/realtime', origin: 'https://marketpulse.test', protocol: 'x402', requestHash: 'hash',
  };
  assert.doesNotThrow(() => validatePayeeOperation(operation));
  assert.equal(operationContext(operation, 'challenge').idempotency_key, 'run.realtime.challenge');
  assert.equal(operationContext(operation, 'settle').idempotency_key, 'run.realtime.settle');
});
