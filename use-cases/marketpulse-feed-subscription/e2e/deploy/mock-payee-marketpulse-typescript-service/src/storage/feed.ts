import type {Task} from '@a2a-js/sdk';
import type {TaskStore} from '@a2a-js/sdk/server';
import { DatabaseSync } from 'node:sqlite';
import type { SettledIncomingPayment } from '@agentvault/sdk';

const ACTORS = [
  ['Donald Trump', 'United States president'], ['Marco Rubio', 'United States secretary of state'],
  ['Ursula von der Leyen', 'European Commission president'], ['Keir Starmer', 'United Kingdom prime minister'],
  ['Emmanuel Macron', 'French president'], ['Friedrich Merz', 'German chancellor'],
  ['Shigeru Ishiba', 'Japanese prime minister'], ['Narendra Modi', 'Indian prime minister'],
  ['Luiz Inacio Lula da Silva', 'Brazilian president'], ['Anthony Albanese', 'Australian prime minister'],
] as const;
const TOPICS = [
  ['tariff', 'proposed reciprocal tariff review for strategic imports', 'SPY'],
  ['chips', 'outlined semiconductor export-control consultations', 'SOXX'],
  ['energy', 'commented on energy-security supply coordination', 'XLE'],
  ['shipping', 'addressed Red Sea shipping-risk countermeasures', 'BDRY'],
  ['defense', 'discussed allied defense procurement commitments', 'ITA'],
  ['currency', 'responded to cross-border currency volatility', 'DXY'],
  ['sanctions', 'announced a review of targeted trade sanctions', 'EEM'],
  ['agriculture', 'set out agricultural import quota negotiations', 'DBA'],
  ['critical-minerals', 'signed a critical-minerals supply memorandum', 'LIT'],
  ['digital-trade', 'opened digital-services tax and AI trade talks', 'QQQ'],
] as const;

export interface NewsItem {
  id: string;
  observedAt: string;
  author: string;
  role: string;
  topic: string;
  symbols: string[];
  headline: string;
  source: string;
  sentimentBps: number;
}

export interface DeliveryState {
  intentId?: string;
  response?: string;
}

export class FeedSqliteRepository implements TaskStore {
  private readonly database: DatabaseSync;

  constructor(path: string) {
    if (!path || path === ':memory:') throw new Error('MARKETPULSE_DATABASE must be persistent');
    this.database = new DatabaseSync(path);
    this.database.exec(`
      PRAGMA journal_mode=DELETE; PRAGMA synchronous=FULL;
      CREATE TABLE IF NOT EXISTS a2a_task(id TEXT PRIMARY KEY, task_json TEXT NOT NULL);
      CREATE TABLE IF NOT EXISTS feed_item(
        id TEXT PRIMARY KEY,observed_at TEXT NOT NULL,author TEXT NOT NULL,role TEXT NOT NULL,topic TEXT NOT NULL,
        symbols_json TEXT NOT NULL,headline TEXT NOT NULL,source TEXT NOT NULL,sentiment_bps INTEGER NOT NULL,
        access_tier TEXT NOT NULL CHECK(access_tier IN ('yesterday','realtime')));
      CREATE TABLE IF NOT EXISTS mcp_delivery(
        run_id TEXT PRIMARY KEY,request_hash TEXT NOT NULL,intent_id TEXT NOT NULL,settlement_json TEXT NOT NULL,
        response_json TEXT,status TEXT NOT NULL CHECK(status IN ('settled','delivered')),
        failure_injections INTEGER NOT NULL DEFAULT 0 CHECK(failure_injections IN (0,1)),updated_at INTEGER NOT NULL);`);
    this.seed();
  }

  async load(taskId:string):Promise<Task|undefined> {
    const row = this.database.prepare('SELECT task_json FROM a2a_task WHERE id=?').get(taskId) as {task_json:string}|undefined;
    return row ? JSON.parse(row.task_json) as Task : undefined;
  }

  async save(task:Task):Promise<void> {
    this.database.prepare('INSERT INTO a2a_task VALUES(?,?) ON CONFLICT(id) DO UPDATE SET task_json=excluded.task_json')
      .run(task.id, JSON.stringify(task));
  }

  close(): void { this.database.close(); }

  private seed(): void {
    const count = Number((this.database.prepare('SELECT COUNT(*) AS count FROM feed_item').get() as { count: number }).count);
    if (count === 100) return;
    if (count !== 0) throw new Error(`MarketPulse feed seed must contain exactly 100 rows, found ${count}`);
    const insert = this.database.prepare('INSERT INTO feed_item VALUES(?,?,?,?,?,?,?,?,?,?)');
    this.database.exec('BEGIN IMMEDIATE');
    try {
      for (let index = 0; index < 100; index += 1) {
        const [actor, role] = ACTORS[index % 10]!;
        const [topic, event, symbol] = TOPICS[Math.floor(index / 10) % 10]!;
        const realtime = index >= 50;
        insert.run(
          `marketpulse-${String(index).padStart(3, '0')}`,
          `2026-10-${realtime ? '15' : '14'}T${String(index % 24).padStart(2, '0')}:${String(index % 60).padStart(2, '0')}:00Z`,
          actor, role, topic, JSON.stringify([symbol]),
          `${actor} ${event}; markets assess geopolitical and tariff implications`,
          realtime ? 'MarketPulse live wire' : 'MarketPulse public archive',
          (index - 50) * 7, realtime ? 'realtime' : 'yesterday',
        );
      }
      this.database.exec('COMMIT');
    } catch (error) {
      this.database.exec('ROLLBACK');
      throw error;
    }
  }

  news(tier: 'yesterday' | 'realtime', limit: number): NewsItem[] {
    const rows = this.database.prepare(`SELECT id,observed_at,author,role,topic,symbols_json,headline,source,sentiment_bps
      FROM feed_item WHERE access_tier=? ORDER BY id LIMIT ?`).all(tier, limit) as Record<string, unknown>[];
    return rows.map((row) => ({
      id: String(row.id), observedAt: String(row.observed_at), author: String(row.author), role: String(row.role),
      topic: String(row.topic), symbols: JSON.parse(String(row.symbols_json)) as string[], headline: String(row.headline),
      source: String(row.source), sentimentBps: Number(row.sentiment_bps),
    }));
  }

  delivery(runId: string, requestHash: string): DeliveryState {
    const row = this.database.prepare(`SELECT request_hash,intent_id,COALESCE(response_json,'') AS response,status
      FROM mcp_delivery WHERE run_id=?`).get(runId) as Record<string, unknown> | undefined;
    if (!row) return {};
    if (row.request_hash !== requestHash || !['settled', 'delivered'].includes(String(row.status))) {
      throw new Error('MCP run_id conflicts with durable delivery');
    }
    return { intentId: String(row.intent_id), response: String(row.response) || undefined };
  }

  recordSettled(runId: string, requestHash: string, payment: SettledIncomingPayment): void {
    if (payment.intent.run_id !== runId) throw new Error('settlement is not bound to the workflow run');
    const intentId = payment.intent.id;
    this.database.prepare(`INSERT INTO mcp_delivery
      (run_id,request_hash,intent_id,settlement_json,response_json,status,updated_at)
      VALUES(?,?,?,?,NULL,'settled',?) ON CONFLICT(run_id) DO NOTHING`).run(
        runId, requestHash, intentId, JSON.stringify({ intentId, runId }), this.now());
    if (this.delivery(runId, requestHash).intentId !== intentId) {
      throw new Error('MCP run_id is already bound to another settlement');
    }
  }

  recordDelivered(runId: string, requestHash: string, response: string): void {
    const result = this.database.prepare(`UPDATE mcp_delivery SET response_json=?,status='delivered',updated_at=?
      WHERE run_id=? AND request_hash=? AND status='settled'`).run(response, this.now(), runId, requestHash);
    if (result.changes !== 1) throw new Error('MCP delivery was not in settled state');
  }

  recordFailure(runId: string, requestHash: string): boolean {
    const result = this.database.prepare(`UPDATE mcp_delivery SET failure_injections=1,updated_at=?
      WHERE run_id=? AND request_hash=? AND status='settled' AND failure_injections=0`).run(
        this.now(), runId, requestHash);
    return result.changes === 1;
  }

  private now(): number { return Math.floor(Date.now() / 1000); }
}
