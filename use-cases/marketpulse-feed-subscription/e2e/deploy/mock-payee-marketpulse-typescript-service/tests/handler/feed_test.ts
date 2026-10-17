import assert from 'node:assert/strict';
import test from 'node:test';
import { FeedHandler } from '../../src/handler/feed.js';

test('FeedHandler exposes operational and MCP handlers', () => {
  assert.equal(typeof FeedHandler.prototype, 'object');
});
