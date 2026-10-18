import test from 'node:test';
import assert from 'node:assert/strict';
import { ApiError, readBounded } from '../../../src/agentvault/transport/transport.js';

test('transport error preserves status and code', () => {
  const error = new ApiError(409, 'RUN_CONFLICT', 'conflict');
  assert.equal(error.status, 409);
  assert.equal(error.code, 'RUN_CONFLICT');
});

test('bounded reader rejects oversized responses', async () => {
  await assert.rejects(readBounded(new Response('oversized'), 2), /configured limit/);
});
