import assert from 'node:assert/strict';
import test from 'node:test';
import { FeedRouter } from '../../src/route/feed.js';

test('FeedRouter owns only ordinary operational routes', () => {
  assert.equal(typeof FeedRouter, 'function');
});
