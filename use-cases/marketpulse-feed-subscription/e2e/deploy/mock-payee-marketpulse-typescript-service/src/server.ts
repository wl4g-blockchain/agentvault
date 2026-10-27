import {
  AgentVault,
  ExpressPayeeMiddleware,
  GrpcConfig,
  MqttConfig,
  type VaultTransportConfig,
  mcpToolRouteWithProtocol,
} from '@agentvault/sdk';
import express, { type Express } from 'express';
import type { Server as HttpServer } from 'node:http';
import { FeedHandler } from './handler/feed.js';
import { FeedRouter } from './route/feed.js';
import { FeedSqliteRepository } from './storage/feed.js';

/** Owns the Express listener, MCP endpoint, SDK middleware and lifecycle. */
export class MarketPulseServer {
  private constructor(
    readonly application: Express,
    private readonly storage: FeedSqliteRepository,
    private readonly vault: AgentVault,
    private readonly host: string,
    private readonly port: number,
  ) {}

  static fromEnvironment(): MarketPulseServer {
    const [host, port] = parseBind(required('MARKETPULSE_BIND'));
    const vault = payeeVault();
    const payee = vault.payee();
    const storage = new FeedSqliteRepository(required('MARKETPULSE_DATABASE'));
    const taskStore = storage;
    const middleware: ExpressPayeeMiddleware = payee.express([mcpToolRouteWithProtocol(
      '/mcp',
      'marketpulse.realtime_news',
      required('MARKETPULSE_SCENARIO_ID'),
      required('MARKETPULSE_PRICE_ID'),
      required('MARKETPULSE_RESOURCE_URL'),
      required('MARKETPULSE_ORIGIN'),
      process.env.MARKETPULSE_PAYMENT_PROTOCOL ?? 'x402',
    ), {method:'POST', path:'/a2a', scenarioId:required('MARKETPULSE_SCENARIO_ID'),
      priceId:process.env.MARKETPULSE_A2A_PRICE_ID ?? required('MARKETPULSE_PRICE_ID'),
      resource:process.env.MARKETPULSE_A2A_RESOURCE_URL ?? required('MARKETPULSE_RESOURCE_URL').replace(/\/mcp$/, '/a2a'),
      origin:required('MARKETPULSE_ORIGIN'), protocol:process.env.MARKETPULSE_PAYMENT_PROTOCOL ?? 'x402',
      a2aTaskStore:taskStore}]);
    const handler = new FeedHandler(
      storage,
      process.env.MARKETPULSE_E2E_ALLOW_FAILURE_INJECTION === 'true', taskStore,
    );
    const application = express();
    application.disable('x-powered-by');
    application.use(...middleware.handlers());
    new FeedRouter(handler).register(application);
    application.post('/mcp', handler.mcp);
    application.post('/a2a', handler.a2a);
    return new MarketPulseServer(application, storage, vault, host, port);
  }

  run(): HttpServer {
    return this.application.listen(this.port, this.host, () => {
      console.log(`MarketPulse TypeScript ready on ${this.host}:${this.port}`);
    });
  }

  async close(): Promise<void> {
    try {
      await this.vault.close();
    } finally {
      this.storage.close();
    }
  }
}

export function required(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required`);
  return value;
}

export function parseBind(bind: string): readonly [string, number] {
  const separator = bind.lastIndexOf(':');
  if (separator <= 0) throw new Error('MARKETPULSE_BIND must be host:port');
  const host = bind.slice(0, separator);
  const port = Number(bind.slice(separator + 1));
  if (!Number.isInteger(port) || port <= 0 || port > 65535) {
    throw new Error('MARKETPULSE_BIND port is out of range');
  }
  return [host, port] as const;
}

function payeeVault(): AgentVault {
  const transport = (process.env.AGENTVAULT_TRANSPORT ?? 'mqtt').trim().toLowerCase();
  let config: VaultTransportConfig;
  if (transport === 'mqtt') {
    config = { provider: 'mqtt', mqtt: new MqttConfig(
      required('AGENTVAULT_MQTT_HOST'),
      Number(required('AGENTVAULT_MQTT_PORT')),
      boolean('AGENTVAULT_MQTT_TLS'),
      required('AGENTVAULT_MQTT_USERNAME'),
      required('AGENTVAULT_MQTT_PASSWORD'),
      'marketpulse-typescript',
      required('AGENTVAULT_MQTT_CLIENT_ID'),
      Number(required('AGENTVAULT_MQTT_TIMEOUT_SECONDS')) * 1_000,
    ) };
  } else if (transport === 'grpc') {
    config = { provider: 'grpc', grpc: new GrpcConfig(
      required('AGENTVAULT_GRPC_ENDPOINT'),
      boolean('AGENTVAULT_GRPC_TLS'),
      undefined,
      Number(required('AGENTVAULT_GRPC_TIMEOUT_SECONDS')) * 1_000,
    ) };
  } else {
    throw new Error('AGENTVAULT_TRANSPORT must be mqtt or grpc');
  }
  return AgentVault.fromConfig(config);
}

function boolean(name: string): boolean {
  const value = (process.env[name] ?? 'false').trim().toLowerCase();
  if (value !== 'true' && value !== 'false') throw new Error(`${name} must be true or false`);
  return value === 'true';
}
