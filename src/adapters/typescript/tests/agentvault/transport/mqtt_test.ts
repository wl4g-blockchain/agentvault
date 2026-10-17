import assert from 'node:assert/strict';
import { inspect } from 'node:util';
import test from 'node:test';
import { MqttConfig } from '../../../src/agentvault/config/config.js';

test('MqttConfig rejects topic injection and redacts credentials', () => {
  assert.throws(() => new MqttConfig('localhost', 1883, false, 'user', 'pass', 'worker', 'a/../b'));
  const config = new MqttConfig('localhost', 1883, false, 'secret-user', 'secret-pass', 'worker', 'client-a');
  assert.doesNotMatch(inspect(config), /secret-user|secret-pass/);
});
