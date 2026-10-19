import {
  AgentVault,
  ExpressPayeeMiddleware,
  GrpcConfig,
  GrpcVaultTransport,
  MqttConfig,
  MqttVaultTransport,
  type VaultTransport,
  mcpToolRoute,
} from '@agentvault/sdk';
import express, { type Express } from 'express';
import type { Server as HttpServer } from 'node:http';
import { FeedHandler } from './handler/feed.js';
import { FeedRouter } from './route/feed.js';
import { FeedSQLiteStorage } from './storage/feed.js';

/** Owns the Express listener, MCP endpoint, SDK middleware and lifecycle. */
export class MarketPulseServer {
  private constructor(
    readonly application: Express,
    private readonly storage: FeedSQLiteStorage,
    private readonly host: string,
    private readonly port: number,
  ) {}

  static fromEnvironment(): MarketPulseServer {
    const [host, port] = parseBind(required('MARKETPULSE_BIND'));
    const transport = payeeTransport();
    const payee = new AgentVault(transport).payee();
    const middleware: ExpressPayeeMiddleware = payee.express([mcpToolRoute(
      '/mcp',
      'marketpulse.realtime_news',
      required('MARKETPULSE_SCENARIO_ID'),
      required('MARKETPULSE_OFFER_ID'),
      required('MARKETPULSE_RESOURCE_URL'),
      required('MARKETPULSE_ORIGIN'),
    )]);
    const storage = new FeedSQLiteStorage(required('MARKETPULSE_DATABASE'));
    const handler = new FeedHandler(
      storage,
      process.env.MARKETPULSE_E2E_ALLOW_FAILURE_INJECTION === 'true',
    );
    const application = express();
    application.disable('x-powered-by');
    application.use(...middleware.handlers());
    new FeedRouter(handler).register(application);
    application.post('/mcp', handler.mcp);
    return new MarketPulseServer(application, storage, host, port);
  }

  run(): HttpServer {
    return this.application.listen(this.port, this.host, () => {
      console.log(`MarketPulse TypeScript ready on ${this.host}:${this.port}`);
    });
  }

  close(): void {
    this.storage.close();
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

function payeeTransport(): VaultTransport {
  const transport = (process.env.AGENTVAULT_TRANSPORT ?? 'mqtt').trim().toLowerCase();
  if (transport === 'mqtt') {
    return new MqttVaultTransport(new MqttConfig(
      required('AGENTVAULT_MQTT_HOST'),
      Number(required('AGENTVAULT_MQTT_PORT')),
      boolean('AGENTVAULT_MQTT_TLS'),
      required('AGENTVAULT_MQTT_USERNAME'),
      required('AGENTVAULT_MQTT_PASSWORD'),
      'marketpulse-typescript',
      required('AGENTVAULT_MQTT_CLIENT_ID'),
      Number(required('AGENTVAULT_MQTT_TIMEOUT_SECONDS')) * 1_000,
    ));
  }
  if (transport === 'grpc') {
    return new GrpcVaultTransport(new GrpcConfig(
      required('AGENTVAULT_GRPC_ENDPOINT'),
      boolean('AGENTVAULT_GRPC_TLS'),
      undefined,
      Number(required('AGENTVAULT_GRPC_TIMEOUT_SECONDS')) * 1_000,
    ));
  }
  throw new Error('AGENTVAULT_TRANSPORT must be mqtt or grpc');
}

function boolean(name: string): boolean {
  const value = (process.env[name] ?? 'false').trim().toLowerCase();
  if (value !== 'true' && value !== 'false') throw new Error(`${name} must be true or false`);
  return value === 'true';
}
