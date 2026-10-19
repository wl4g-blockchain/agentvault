import assert from 'node:assert/strict';
import test from 'node:test';
import { GrpcConfig } from '../../../src/agentvault/config/config.js';

test('GrpcConfig rejects cleartext non-loopback endpoints', () => {
  assert.throws(() => new GrpcConfig('vault.example:50051', false));
  assert.doesNotThrow(() => new GrpcConfig('127.0.0.1:50051', false));
});
