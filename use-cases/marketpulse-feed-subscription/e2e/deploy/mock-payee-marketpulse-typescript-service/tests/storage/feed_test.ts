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

test('native A2A Task payment scope survives a repository reopen', async () => {
  const directory = mkdtempSync(join(tmpdir(), 'marketpulse-ts-a2a-'));
  const path = join(directory, 'marketpulse.sqlite3');
  let storage = new FeedSqliteRepository(path);
  await storage.save({kind:'task', id:'task-1', contextId:'context-1', status:{state:'submitted'},
    metadata:{'agentvault.payment.run_id':'run-1'}});
  storage.close();
  storage = new FeedSqliteRepository(path);
  try {
    const task = await storage.load('task-1');
    assert.equal(task?.contextId, 'context-1');
    assert.equal(task?.metadata?.['agentvault.payment.run_id'], 'run-1');
  } finally {storage.close(); rmSync(directory, {recursive:true});}
});
