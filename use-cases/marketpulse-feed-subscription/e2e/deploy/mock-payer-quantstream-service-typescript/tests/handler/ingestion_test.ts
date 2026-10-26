import assert from 'node:assert/strict';
import test from 'node:test';
import { IngestionHandler } from '../../src/handler/ingestion.js';

test('IngestionHandler exposes only operational HTTP handlers', () => {
  const handler = new IngestionHandler();
  assert.equal(typeof handler.health, 'function');
  assert.equal(typeof handler.metrics, 'function');
  assert.equal('run' in handler, false);
});
