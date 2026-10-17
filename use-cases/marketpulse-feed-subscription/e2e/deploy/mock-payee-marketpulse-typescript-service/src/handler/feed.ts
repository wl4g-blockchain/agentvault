import { createHash } from 'node:crypto';
import type { SettledIncomingPayment } from '@agentvault/sdk';
import type { Request, RequestHandler, Response } from 'express';
import type { FeedSQLiteStorage, NewsItem } from '../storage/feed.js';

interface RpcRequest {
  jsonrpc?: string;
  id?: unknown;
  method?: string;
  params?: Record<string, unknown>;
}

export class FeedHandler {
  constructor(
    private readonly storage: FeedSQLiteStorage,
    private readonly allowFailureInjection: boolean,
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
    const runId = request.get('x-run-id')?.trim();
    if (!runId) {
      response.status(400).json(this.error(id, -32602, 'x-run-id is required'));
      return;
    }
    const requestHash = createHash('sha256')
      .update('marketpulse.mcp.request.v1\0')
      .update(request.body as Buffer)
      .digest('hex');
    let delivery;
    try {
      delivery = this.storage.delivery(runId, requestHash);
    } catch (failure) {
      response.status(409).json(this.error(id, -32009, String(failure)));
      return;
    }
    if (delivery.response) {
      response.type('application/json').send(delivery.response);
      return;
    }
    let intentId = delivery.intentId;
    if (!intentId) {
      const payment = response.locals.agentvaultPayment as SettledIncomingPayment | undefined;
      if (!payment) {
        response.status(402).json(this.error(id, -32005, 'settled payment is required'));
        return;
      }
      this.storage.recordSettled(runId, requestHash, payment);
      intentId = payment.intent.id;
    }
    if (this.allowFailureInjection && request.get('x-e2e-fail-after-settlement') === 'once' &&
        this.storage.recordFailure(runId, requestHash)) {
      response.status(503).json(this.error(id, -32004, 'injected delivery failure after settlement'));
      return;
    }
    const encoded = JSON.stringify(this.toolResult(id, this.storage.news('realtime', 20), true, intentId));
    this.storage.recordDelivered(runId, requestHash, encoded);
    response.type('application/json').send(encoded);
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
