import assert from 'node:assert/strict';
import test from 'node:test';
import { GrpcConfig } from '../../../src/agentvault/config/config.js';
import { GrpcVaultTransport } from '../../../src/agentvault/transport/grpc.js';
import { VaultTransportFactory } from '../../../src/agentvault/transport/factory.js';

test('transport factory creates isolated idempotently closable providers', async () => {
  const factory = new VaultTransportFactory();
  const config = { provider: 'grpc' as const, grpc: new GrpcConfig('127.0.0.1:65535', false) };
  const first = factory.create(config);
  const second = factory.create(config);
  assert.ok(first instanceof GrpcVaultTransport);
  assert.ok(second instanceof GrpcVaultTransport);
  assert.notStrictEqual(first, second);
  await first.close();
  await first.close();
  await second.close();
});
