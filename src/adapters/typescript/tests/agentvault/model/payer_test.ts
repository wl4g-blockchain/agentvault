import assert from 'node:assert/strict';
import test from 'node:test';
import { PayerRequestContext } from '../../../src/agentvault/model/payer.js';

test('payer context is stable for one logical operation', () => {
  const first = new PayerRequestContext('run-1');
  const retry = new PayerRequestContext('run-1');
  assert.deepEqual(first, retry);
  assert.equal(first.runId, 'run-1');
});
