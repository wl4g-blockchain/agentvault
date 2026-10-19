import assert from 'node:assert/strict';
import test from 'node:test';
import { parseBind, required } from '../src/server.js';

test('server validates listener and required settings', () => {
  assert.deepEqual(parseBind('127.0.0.1:8081'), ['127.0.0.1', 8081]);
  assert.throws(() => parseBind('invalid'));
  delete process.env.MARKETPULSE_TYPESCRIPT_MISSING;
  assert.throws(() => required('MARKETPULSE_TYPESCRIPT_MISSING'));
});
