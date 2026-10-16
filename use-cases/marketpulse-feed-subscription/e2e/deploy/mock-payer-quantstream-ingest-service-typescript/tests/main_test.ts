import assert from 'node:assert/strict';
import test from 'node:test';
import { main } from '../src/main.js';

test('main rejects undeclared command-line arguments before runtime startup', async () => {
  const original = process.argv;
  process.argv = [original[0] ?? 'node', original[1] ?? 'main.ts', '--unknown'];
  try {
    await assert.rejects(main(), /usage: quantstream-typescript/);
  } finally {
    process.argv = original;
  }
});
