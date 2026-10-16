import assert from 'node:assert/strict';
import { inspect } from 'node:util';
import test from 'node:test';
import { GrpcConfig, MqttConfig } from '../../../src/agentvault/config/config.js';

test('role configuration validates boundaries and redacts edge secrets', () => {
  assert.throws(() => new GrpcConfig('vault.example:50051', false));
  const grpc = new GrpcConfig('vault.example:443', true, 'edge-secret');
  assert.doesNotMatch(inspect(grpc), /edge-secret/);
  const mqtt = new MqttConfig('127.0.0.1', 1883, false, 'client', 'mqtt-secret', 'worker', 'client-a');
  assert.doesNotMatch(inspect(mqtt), /mqtt-secret/);
});
