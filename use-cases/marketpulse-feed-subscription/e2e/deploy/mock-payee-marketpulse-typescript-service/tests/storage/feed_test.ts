import assert from 'node:assert/strict';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { FeedSqliteRepository } from '../../src/storage/feed.js';

test('FeedSqliteRepository initializes exactly one hundred rows', () => {
  const directory = mkdtempSync(join(tmpdir(), 'marketpulse-ts-'));
  const storage = new FeedSqliteRepository(join(directory, 'marketpulse.sqlite3'));
  assert.equal(storage.news('yesterday', 100).length, 50);
  assert.equal(storage.news('realtime', 100).length, 50);
  storage.close();
  rmSync(directory, { recursive: true });
});
