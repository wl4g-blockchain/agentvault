import assert from 'node:assert/strict';
import { inspect } from 'node:util';
import test from 'node:test';
import { HttpVaultTransport } from '../../../src/agentvault/transport/http.js';

test('HttpVaultTransport redacts the AuthGuard bearer', () => {
  const transport = new HttpVaultTransport('https://vault.test', 'bearer-secret');
  assert.doesNotMatch(inspect(transport), /bearer-secret/);
});
