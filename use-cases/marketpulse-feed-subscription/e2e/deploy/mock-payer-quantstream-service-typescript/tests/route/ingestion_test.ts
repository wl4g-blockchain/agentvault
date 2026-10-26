import assert from 'node:assert/strict';
import test from 'node:test';
import { IngestionRouter } from '../../src/route/ingestion.js';

test('IngestionRouter exposes health and rejects undeclared paths', async () => {
  const server = new IngestionRouter().app().listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', resolve));
  try {
    const address = server.address();
    if (!address || typeof address === 'string') throw new Error('server did not bind a TCP port');
    const health = await fetch(`http://127.0.0.1:${address.port}/healthz`);
    const metrics = await fetch(`http://127.0.0.1:${address.port}/metrics`);
    const manual = await fetch(`http://127.0.0.1:${address.port}/api/v1/ingestion`, { method: 'POST' });
    const missing = await fetch(`http://127.0.0.1:${address.port}/unknown`);
    assert.equal(health.status, 200);
    assert.deepEqual(await health.json(), { status: 'ready', service: 'quantstream-typescript' });
    assert.equal(metrics.status, 200);
    assert.equal(manual.status, 404);
    assert.equal(missing.status, 404);
  } finally {
    await new Promise<void>((resolve, reject) => server.close((error) => error ? reject(error) : resolve()));
  }
});
