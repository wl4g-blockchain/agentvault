import type {TaskStore} from '@a2a-js/sdk/server';
import type {MessageSendParams} from '@a2a-js/sdk';
import { createHash } from 'node:crypto';
import type { SettledIncomingPayment } from '@agentvault/sdk';
import type { Request, RequestHandler, Response } from 'express';
import type { FeedSqliteRepository, NewsItem } from '../storage/feed.js';

interface RpcRequest {
  jsonrpc?: string;
  id?: unknown;
  method?: string;
  params?: Record<string, unknown>;
}

export class FeedHandler {
  constructor(
    private readonly storage: FeedSqliteRepository,
    private readonly allowFailureInjection: boolean,
    private readonly taskStore?: TaskStore,
  ) {}

  readonly health: RequestHandler = (_request, response) => {
    response.json({ status: 'ready', service: 'marketpulse-typescript' });
  };

  readonly metrics: RequestHandler = (_request, response) => {
    response.type('text/plain; version=0.0.4').send(
      '# HELP marketpulse_up MarketPulse process readiness.\n' +
      '# TYPE marketpulse_up gauge\nmarketpulse_up 1\n',
    );
  };

  readonly mcp: RequestHandler = (request, response) => {
    if (!Buffer.isBuffer(request.body)) {
      response.status(400).json(this.error(null, -32600, 'invalid request'));
      return;
    }
    let rpc: RpcRequest;
    try {
      rpc = JSON.parse(request.body.toString('utf8')) as RpcRequest;
    } catch {
      response.status(400).json(this.error(null, -32600, 'invalid request'));
      return;
    }
    if (rpc.jsonrpc !== '2.0') {
      response.status(400).json(this.error(rpc.id, -32600, 'invalid request'));
      return;
    }
    const params = rpc.params ?? {};
    switch (rpc.method) {
      case 'initialize':
        response.json(this.result(rpc.id, {
          protocolVersion: typeof params.protocolVersion === 'string' ? params.protocolVersion : '2025-11-25',
          capabilities: { tools: { listChanged: false } },
          serverInfo: { name: 'marketpulse-feed-typescript', version: '0.1.0' },
        }));
        return;
      case 'notifications/initialized':
        response.sendStatus(202);
        return;
      case 'tools/list':
        response.json(this.result(rpc.id, { tools: this.tools() }));
        return;
      case 'tools/call':
        this.callTool(request, response, rpc.id, params);
        return;
      default:
        response.status(404).json(this.error(rpc.id, -32601, 'method not found'));
    }
  };

  private callTool(request: Request, response: Response, id: unknown, params: Record<string, unknown>): void {
    if (params.name === 'marketpulse.yesterday_news') {
      response.json(this.toolResult(id, this.storage.news('yesterday', 20), false));
      return;
    }
    if (params.name !== 'marketpulse.realtime_news') {
      response.status(404).json(this.error(id, -32602, 'unknown tool'));
      return;
    }
    const delivery = this.paidNews(request, id, request.body as Buffer, response.locals.agentvaultPayment);
    response.status(delivery.status).type('application/json').send(delivery.body);
  }

  readonly a2a: RequestHandler = async (request, response) => {
    const rpc = JSON.parse((request.body as Buffer).toString('utf8')) as RpcRequest;
    const params = rpc.params as unknown as MessageSendParams;
    const task = params?.message.taskId ? await this.taskStore?.load(params.message.taskId) : undefined;
    if (rpc.method !== 'message/send' || !task) {
      response.status(409).json(this.error(rpc.id, -32009, 'payment Task is missing'));
      return;
    }
    const canonical = Buffer.from(JSON.stringify({jsonrpc:'2.0', id:request.get('x-run-id'),
      method:'tools/call', params:{name:'marketpulse.realtime_news', arguments:{}}}));
    const delivery = this.paidNews(request, rpc.id, canonical, response.locals.agentvaultPayment);
    const value = JSON.parse(delivery.body) as {error?:unknown; result?:{structuredContent:Record<string,unknown>}};
    if (value.error || !value.result) {
      response.status(delivery.status).type('application/json').send(delivery.body);
      return;
    }
    task.status = {state:'completed'};
    task.artifacts = [{artifactId:'realtime-news', parts:[{kind:'data', data:value.result.structuredContent}]}];
    await this.taskStore!.save(task);
    response.json(this.result(rpc.id, task));
  };

  private paidNews(request: Request, id: unknown, body: Buffer, payment?: SettledIncomingPayment): {status:number; body:string} {
    const failure = (status:number, code:number, message:string) => ({status, body:JSON.stringify(this.error(id, code, message))});
    const runId = request.get('x-run-id')?.trim();
    if (!runId) return failure(400, -32602, 'x-run-id is required');
    const requestHash = createHash('sha256').update('marketpulse.mcp.request.v1\0').update(body).digest('hex');
    let delivery;
    try { delivery = this.storage.delivery(runId, requestHash); }
    catch (error) { return failure(409, -32009, String(error)); }
    if (delivery.response) return {status:200, body:delivery.response};
    let intentId = delivery.intentId;
    if (!intentId) {
      if (!payment) return failure(402, -32005, 'settled payment is required');
      this.storage.recordSettled(runId, requestHash, payment);
      intentId = payment.intent.id;
    }
    if (this.allowFailureInjection && request.get('x-e2e-fail-after-settlement') === 'once' && this.storage.recordFailure(runId, requestHash)) {
      return failure(503, -32004, 'injected delivery failure after settlement');
    }
    const encoded = JSON.stringify(this.toolResult(id, this.storage.news('realtime', 20), true, intentId));
    this.storage.recordDelivered(runId, requestHash, encoded);
    return {status:200, body:encoded};
  }

  private tools(): unknown[] {
    const inputSchema = { type: 'object', additionalProperties: false };
    return [
      { name: 'marketpulse.yesterday_news', description: 'Free previous-day geopolitical and tariff intelligence', inputSchema },
      { name: 'marketpulse.realtime_news', description: 'Paid real-time geopolitical and tariff intelligence via x402', inputSchema },
    ];
  }

  private toolResult(id: unknown, news: NewsItem[], paid: boolean, intentId?: string): unknown {
    const structuredContent = { news, paid, intentId: intentId ?? null };
    return this.result(id, {
      content: [{ type: 'text', text: JSON.stringify(structuredContent) }], structuredContent, isError: false,
    });
  }

  private result(id: unknown, result: unknown): unknown {
    return { jsonrpc: '2.0', id: id ?? null, result };
  }

  private error(id: unknown, code: number, message: string): unknown {
    return { jsonrpc: '2.0', id: id ?? null, error: { code, message } };
  }
}
