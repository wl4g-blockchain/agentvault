import { randomBytes } from 'node:crypto';
import { isIP } from 'node:net';
import { inspect } from 'node:util';
import mqtt, { type MqttClient } from 'mqtt';
import { MqttConfig } from '../config/config.js';
import { ApiError, type VaultTransport } from './http.js';

const AUTHORIZE_PATH = '/api/v1/payments/authorize';
const OUTBOUND_RESPONSE_PATH = /^\/api\/v1\/payments\/([^/?#]+)\/outbound-response$/;
const CORRELATION = /^[0-9a-f]{32}$/;

interface WireResponse {
  type: 'payment.authorization_result' | 'payment.outbound_response_result' | 'payment.offer_resolved' |
    'payment.challenge_created' | 'payment.verification_result' | 'payment.settlement_result' | 'payment.error';
  version: 1;
  correlationId: string;
  status: number;
  headers: Record<string, string>;
  body: string;
}

interface PendingRequest {
  expectedType: WireResponse['type'];
  resolve: (value: WireResponse) => void;
  reject: (error: Error) => void;
  timer: NodeJS.Timeout;
}

export class MqttVaultTransport implements VaultTransport {
  private client: MqttClient | undefined;
  private readonly prefix: string;
  private readonly pending = new Map<string, PendingRequest>();

  constructor(public readonly config: MqttConfig) {
    this.prefix = `agentvault/v1/clients/${config.vaultClientId}`;
  }

  [inspect.custom](): string {
    return `MqttVaultTransport { config: ${inspect(this.config)}, state: <redacted> }`;
  }

  async connect(): Promise<void> {
    if (this.client) return;
    const address = isIP(this.config.host) === 6 ? `[${this.config.host}]` : this.config.host;
    const broker = `${this.config.tls ? 'mqtts' : 'mqtt'}://${address}:${this.config.port}`;
    const client = mqtt.connect(broker, {
      clientId: `${this.config.connectionPrefix}-${randomBytes(6).toString('hex')}`,
      username: this.config.username,
      password: this.config.password,
      protocolVersion: 4,
      clean: true,
      reconnectPeriod: 0,
      connectTimeout: this.config.timeoutMs,
    });
    this.client = client;
    client.on('message', (topic, payload) => this.onMessage(topic, payload));
    try {
      await new Promise<void>((resolve, reject) => {
        const timer = setTimeout(() => reject(new Error('MQTT connect timed out')), this.config.timeoutMs);
        client.once('error', (error) => {
          clearTimeout(timer);
          reject(new Error(`MQTT connection failed: ${error.name}`));
        });
        client.once('connect', () => {
          client.subscribe(
            `${this.prefix}/results/+`,
            { qos: 1 },
            (error, granted) => {
              clearTimeout(timer);
              if (error) reject(new Error(`MQTT subscribe failed: ${error.name}`));
              else if (!subscriptionGranted(granted, 1)) {
                reject(new Error('AgentVault MQTT subscriptions were rejected or downgraded'));
              } else resolve();
            },
          );
        });
      });
    } catch (error) {
      this.client = undefined;
      await new Promise<void>((resolve) => client.end(true, {}, () => resolve()));
      throw error;
    }
  }

  async close(): Promise<void> {
    const client = this.client;
    this.client = undefined;
    if (client) await new Promise<void>((resolve) => client.end(false, {}, () => resolve()));
    for (const [correlation, waiting] of this.pending) {
      clearTimeout(waiting.timer);
      waiting.reject(new Error('MQTT transport closed'));
      this.pending.delete(correlation);
    }
  }

  async request<T>(method: 'GET' | 'POST', path: string, body?: unknown): Promise<T> {
    const operation = this.operation(method, path);
    if (body === undefined) throw new Error('MQTT payment commands require a request body');
    if (!this.client?.connected) throw new Error('MQTT transport is not connected');
    if (this.pending.size >= 256) throw new Error('too many pending MQTT payment requests');
    const correlationId = randomBytes(16).toString('hex');
    const headers: Record<string, string> = { accept: 'application/json', 'content-type': 'application/json' };
    const payload = Buffer.from(JSON.stringify({
      type: operation.messageType,
      version: 1,
      correlationId,
      request: {
        method: 'POST',
        path,
        headers,
        body: Buffer.from(JSON.stringify(body)).toString('base64url'),
      },
    }));
    if (payload.length > this.config.maxResponseBytes) {
      throw new Error('MQTT payment request exceeds configured limit');
    }
    const result = new Promise<WireResponse>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(correlationId);
        reject(new Error('AgentVault MQTT request timed out'));
      }, this.config.timeoutMs);
      this.pending.set(correlationId, { expectedType: operation.responseType, resolve, reject, timer });
    });
    this.client.publish(
      `${this.prefix}/commands/${operation.command}`,
      payload,
      { qos: 1, retain: false },
      (error) => {
        if (!error) return;
        const waiting = this.pending.get(correlationId);
        if (waiting) {
          clearTimeout(waiting.timer);
          this.pending.delete(correlationId);
          waiting.reject(new Error(`MQTT publish failed: ${error.name}`));
        }
      },
    );
    const response = await result;
    const decoded = Buffer.from(response.body, 'base64url');
    if (decoded.length > this.config.maxResponseBytes || decoded.toString('base64url') !== response.body) {
      throw new Error('invalid or oversized AgentVault MQTT response body');
    }
    let value: unknown;
    try {
      value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(decoded));
    } catch {
      throw new Error('invalid AgentVault MQTT JSON response');
    }
    if (response.status < 200 || response.status >= 300) {
      const detail = (value as { error?: { code?: string; message?: string } } | undefined)?.error;
      throw new ApiError(
        response.status,
        detail?.code ?? 'HTTP_ERROR',
        detail?.message ?? `AgentVault returned HTTP ${response.status}`,
      );
    }
    return value as T;
  }

  private operation(method: 'GET' | 'POST', path: string): {
    command: string;
    messageType: string;
    responseType: WireResponse['type'];
  } {
    if (method === 'POST' && path === AUTHORIZE_PATH) {
      return { command: 'authorize', messageType: 'payment.authorize', responseType: 'payment.authorization_result' };
    }
    if (method === 'POST' && OUTBOUND_RESPONSE_PATH.test(path)) {
      return {
        command: 'outbound-response',
        messageType: 'payment.outbound_response',
        responseType: 'payment.outbound_response_result',
      };
    }
    const payee: Record<string, {
      command: string;
      messageType: string;
      responseType: WireResponse['type'];
    }> = {
      '/api/v1/offers/resolve': {
        command: 'resolve-offer',
        messageType: 'payment.resolve_offer',
        responseType: 'payment.offer_resolved',
      },
      '/api/v1/payee/challenges': {
        command: 'create-challenge',
        messageType: 'payment.create_challenge',
        responseType: 'payment.challenge_created',
      },
      '/api/v1/payee/verify': {
        command: 'verify-payment',
        messageType: 'payment.verify',
        responseType: 'payment.verification_result',
      },
      '/api/v1/payee/settle': {
        command: 'settle-payment',
        messageType: 'payment.settle',
        responseType: 'payment.settlement_result',
      },
    };
    if (method === 'POST' && payee[path]) return payee[path];
    throw new Error('MQTT transport does not support this payment operation');
  }

  private onMessage(topic: string, payload: Buffer): void {
    if (payload.length > this.config.maxResponseBytes) return;
    let value: Record<string, unknown>;
    try {
      value = JSON.parse(payload.toString('utf8')) as Record<string, unknown>;
    } catch {
      return;
    }
    const correlation = value.correlationId;
    const waiting = typeof correlation === 'string' ? this.pending.get(correlation) : undefined;
    const status = value.status;
    const responseTypeMatches = typeof status === 'number' &&
      ((status >= 400 && value.type === 'payment.error') ||
        (status < 400 && value.type === waiting?.expectedType));
    const headers = value.headers;
    if (!waiting || !responseTypeMatches || value.version !== 1 || typeof correlation !== 'string' ||
        !CORRELATION.test(correlation) || topic !== `${this.prefix}/results/${correlation}` ||
        !Number.isInteger(status) || (status as number) < 100 || (status as number) > 599 ||
        typeof value.body !== 'string' || !headers || typeof headers !== 'object' || Array.isArray(headers) ||
        Object.values(headers).some((item) => typeof item !== 'string') ||
        !['type', 'version', 'correlationId', 'status', 'headers', 'body'].every((key) => key in value) ||
        Object.keys(value).length !== 6) {
      return;
    }
    const response = value as unknown as WireResponse;
    clearTimeout(waiting.timer);
    this.pending.delete(correlation);
    waiting.resolve(response);
  }
}

function subscriptionGranted(grants: ReadonlyArray<{ qos: number }> | undefined, expectedCount: number): boolean {
  return grants?.length === expectedCount && grants.every((grant) => grant.qos === 1);
}
