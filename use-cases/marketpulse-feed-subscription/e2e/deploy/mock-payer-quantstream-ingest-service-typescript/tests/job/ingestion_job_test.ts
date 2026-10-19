import assert from 'node:assert/strict';
import test from 'node:test';
import { IngestionJob } from '../../src/job/ingestion_job.js';

test('ingestion rejects an in-memory business evidence database before transport startup', async () => {
  const previous = process.env.QUANTSTREAM_DATABASE;
  process.env.QUANTSTREAM_DATABASE = ':memory:';
  try {
    await assert.rejects(IngestionJob.fromEnvironment(), /must be persistent/);
  } finally {
    if (previous === undefined) delete process.env.QUANTSTREAM_DATABASE;
    else process.env.QUANTSTREAM_DATABASE = previous;
  }
});
