import assert from 'node:assert/strict';
import test from 'node:test';
import { main } from '../src/main.js';

test('main is a thin MarketPulseServer entrypoint', () => {
  assert.equal(typeof main, 'function');
});
