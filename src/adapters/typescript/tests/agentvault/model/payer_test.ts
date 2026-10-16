import assert from 'node:assert/strict';
import test from 'node:test';
import { PayerRequestContext } from '../../../src/agentvault/model/payer.js';

test('payer context is stable for one logical operation', () => {
  const first = PayerRequestContext.forOperation('run-1', 'realtime');
  const retry = PayerRequestContext.forOperation('run-1', 'realtime');
  assert.deepEqual(first, retry);
  assert.equal(first.idempotencyKey, 'run-1:realtime');
});
