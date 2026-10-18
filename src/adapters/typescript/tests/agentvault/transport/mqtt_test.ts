import assert from 'node:assert/strict';
import { inspect } from 'node:util';
import test from 'node:test';
import { MqttConfig } from '../../../src/agentvault/config/config.js';
import { MqttVaultTransport } from '../../../src/agentvault/transport/mqtt.js';

test('MqttConfig rejects topic injection and redacts credentials', () => {
  assert.throws(() => new MqttConfig('localhost', 1883, false, 'user', 'pass', 'worker', 'a/../b'));
  const config = new MqttConfig('localhost', 1883, false, 'secret-user', 'secret-pass', 'worker', 'client-a');
  assert.doesNotMatch(inspect(config), /secret-user|secret-pass/);
});

test('MqttVaultTransport lazily connects on the first request', async () => {
  const transport = new MqttVaultTransport(
    new MqttConfig('localhost', 1883, false, 'user', 'pass', 'worker', 'client-a'),
  );
  let calls = 0;
  transport.connect = async () => {
    calls += 1;
    throw new Error('lazy-connect');
  };

  await assert.rejects(
    transport.request('POST', '/api/v1/payee/challenges', { runId: 'run-1' }),
    /lazy-connect/,
  );
  assert.equal(calls, 1);
});
