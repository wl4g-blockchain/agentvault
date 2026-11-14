import { DatabaseSync } from 'node:sqlite';
import cron, { type ScheduledTask } from 'node-cron';
import { MCPHelper, required, type ToolResult } from '../agent/mcp.js';

export class IngestionJob {
  private constructor(private readonly mcp: MCPHelper, private readonly database: string,
                      private readonly failAfterSettlement: boolean) {}

  static async fromEnvironment(): Promise<IngestionJob> {
    const database = required('QUANTSTREAM_DATABASE');
    if (database === ':memory:') throw new Error('QUANTSTREAM_DATABASE must be persistent');
    return new IngestionJob(await MCPHelper.fromEnvironment(), database,
      process.env.QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT === 'once');
  }

  async close(): Promise<void> { await this.mcp.close(); }

  // First periodic function: free delayed-news ingestion.
  ingestYesterdayNews(runId: string): Promise<ToolResult> { return this.mcp.yesterdayNews(runId); }

  // Second periodic function: paid realtime-news ingestion.
  ingestRealtimeNews(runId: string, injectFailure = false): Promise<ToolResult> {
    return this.mcp.realtimeNews(runId, injectFailure);
  }

  async runOnce(sequence = 1): Promise<Record<string, unknown>> {
    const runId = `typescript-${Math.floor(Date.now() / 1000)}-${sequence}`;
    const free = await this.ingestYesterdayNews(runId);
    const paid = await this.ingestRealtimeNews(runId, this.failAfterSettlement && sequence === 1);
    persist(this.database, runId, free, paid);
    return { runId, intentId: paid.paymentIntentId, freeStatus: 200, paidStatus: 200 };
  }

  start(): ScheduledTask[] {
    const yesterday = cron.schedule(process.env.QUANTSTREAM_YESTERDAY_CRON ?? '0 2 * * *', () => {
      void this.ingestYesterdayNews(`typescript-yesterday-${Math.floor(Date.now() / 1000)}`)
        .catch((error: unknown) => console.error('delayed-news ingestion failed', error));
    });
    const realtime = cron.schedule(process.env.QUANTSTREAM_REALTIME_CRON ?? '*/1 * * * *', () => {
      void this.ingestRealtimeNews(`typescript-realtime-${Math.floor(Date.now() / 1000)}`)
        .catch((error: unknown) => console.error('realtime-news ingestion failed', error));
    });
    return [yesterday, realtime];
  }
}

export async function runOnce(sequence = 1): Promise<Record<string, unknown>> {
  const job = await IngestionJob.fromEnvironment();
  try { return await job.runOnce(sequence); } finally { await job.close(); }
}

function persist(path: string, runId: string, free: ToolResult, paid: ToolResult): void {
  if (!paid.paymentIntentId) throw new Error('paid result omitted payment intent');
  const decoder = new TextDecoder('utf-8', { fatal: true });
  const database = new DatabaseSync(path);
  try {
    database.exec(`PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL; CREATE TABLE IF NOT EXISTS ingestion_run (
      run_id TEXT PRIMARY KEY, free_response_json TEXT NOT NULL, paid_response_json TEXT NOT NULL,
      payment_intent_id TEXT NOT NULL UNIQUE, completed_at INTEGER NOT NULL)`);
    database.prepare('INSERT INTO ingestion_run VALUES(?,?,?,?,?)').run(runId,
      JSON.stringify(JSON.parse(decoder.decode(free.body))), JSON.stringify(JSON.parse(decoder.decode(paid.body))),
      paid.paymentIntentId, Math.floor(Date.now() / 1000));
  } finally { database.close(); }
}
