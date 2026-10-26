import { randomBytes } from 'node:crypto';
import { isIP } from 'node:net';
import { inspect } from 'node:util';
import mqtt, { type MqttClient } from 'mqtt';
import { MqttConfig } from '../config/config.js';
import { ApiError, type VaultTransport } from './transport.js';

const AUTHORIZE_PATH = '/api/v1/payments/authorize';
const OUTBOUND_RESPONSE_PATH = /^\/api\/v1\/payments\/([^/?#]+)\/outbound-response$/;
const REQUEST_SPAN_ID = /^(?!0{16}$)[0-9a-f]{16}$/;
const TRACEPARENT = /^00-((?!0{32}$)[0-9a-f]{32})-((?!0{16}$)[0-9a-f]{16})-([0-9a-f]{2})$/;

interface WireResponse {
  type: 'payment.authorization_result' | 'payment.outbound_response_result' | 'payment.offer_resolved' |
    'payment.challenge_created' | 'payment.verification_result' | 'payment.settlement_result' | 'payment.error';
  version: 1;
  requestSpanId: string;
  status: number;
  headers: Record<string, string>;
  body: string;
}

interface PendingRequest {
  expectedType: WireResponse['type'];
  commandTopic: string;
  payload: Buffer;
  resolve: (value: WireResponse) => void;
  reject: (error: Error) => void;
  timer: NodeJS.Timeout;
  release: () => void;
}

export class MqttVaultTransport implements VaultTransport {
  private client: MqttClient | undefined;
  private connectPromise: Promise<void> | undefined;
  private readyResult: ((error?: Error) => void) | undefined;
  private readonly prefix: string;
  private readonly pending = new Map<string, PendingRequest>();
  private ready = false;
  private closed = false;

  constructor(public readonly config: MqttConfig) {
    this.prefix = `agentvault/v1/clients/${config.vaultClientId}`;
  }

  [inspect.custom](): string {
    return `MqttVaultTransport { config: ${inspect(this.config)}, state: <redacted> }`;
  }

  async connect(): Promise<void> {
    if (this.closed) throw new Error('MQTT transport closed');
    if (this.ready) return;
    if (this.connectPromise) return this.connectPromise;
    if (!this.client) this.open();
    this.connectPromise = this.waitUntilReady();
    try {
      await this.connectPromise;
    } finally {
      this.connectPromise = undefined;
    }
  }

  private open(): void {
    const address = isIP(this.config.host) === 6 ? `[${this.config.host}]` : this.config.host;
    const broker = `${this.config.tls ? 'mqtts' : 'mqtt'}://${address}:${this.config.port}`;
    const client = mqtt.connect(broker, {
      clientId: `${this.config.connectionPrefix}-${randomBytes(6).toString('hex')}`,
      username: this.config.username,
      password: this.config.password,
      protocolVersion: 4,
      clean: true,
      resubscribe: false,
      reconnectPeriod: 1000,
      connectTimeout: this.config.timeoutMs,
    });
    this.client = client;
    client.on('message', (topic, payload, packet) => {
      if (packet.qos === 1 && !packet.retain) this.onMessage(topic, payload);
    });
    client.on('error', () => {
      // mqtt.js reconnects automatically. Pending requests retain their exact
      // envelope and remain bounded by their own timeout.
    });
    client.on('close', () => { this.ready = false; });
    client.on('offline', () => { this.ready = false; });
    client.on('reconnect', () => { this.ready = false; });
    client.on('connect', () => {
      this.ready = false;
      client.subscribe(
        `${this.prefix}/results/+`,
        { qos: 1 },
        (error, granted) => {
          if (error) { this.ready = false; return; }
          if (!subscriptionGranted(granted, 1)) {
            this.ready = false;
            this.failAll(new Error('AgentVault MQTT result subscription was rejected or downgraded'));
            return;
          }
          if (this.closed) return;
          this.ready = true;
          this.readyResult?.();
          for (const requestSpanId of this.pending.keys()) this.publishPending(requestSpanId);
        },
      );
    });
  }

  private waitUntilReady(): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      const timer = setTimeout(() => this.readyResult?.(new Error('MQTT connect/subscribe timed out')), this.config.timeoutMs);
      this.readyResult = (error) => {
        clearTimeout(timer);
        this.readyResult = undefined;
        if (error) reject(error); else resolve();
      };
      if (this.ready) this.readyResult();
    });
  }

  private failAll(error: Error): void {
    this.readyResult?.(error);
    for (const [id, waiting] of this.pending) {
      this.pending.delete(id);
      clearTimeout(waiting.timer);
      waiting.release();
      waiting.reject(error);
    }
  }

  async close(): Promise<void> {
    if (this.closed) return;
    this.closed = true;
    this.ready = false;
    const client = this.client;
    this.client = undefined;
    this.failAll(new Error('MQTT transport closed'));
    // Graceful end waits forever for missing PUBACKs on a disconnected broker.
    if (client) await new Promise<void>((resolve) => client.end(true, {}, () => resolve()));
  }

  async request<T>(method: 'GET' | 'POST', path: string, body?: unknown, signal?: AbortSignal): Promise<T> {
    const operation = this.operation(method, path);
    if (body === undefined) throw new Error('MQTT payment commands require a request body');
    if (this.closed) throw new Error('MQTT transport closed');
    signal?.throwIfAborted();
    if (this.pending.size >= 256) throw new Error('too many pending MQTT payment requests');
    const trace = producerTraceContext({});
    const requestSpanId = trace.spanId;
    const headers: Record<string, string> = {
      accept: 'application/json',
      'content-type': 'application/json',
      traceparent: trace.traceparent,
    };
    console.info(JSON.stringify({
      event: 'agentvault.sdk.transport.mqtt.produced',
      traceId: trace.traceId,
      spanId: trace.spanId,
      parentId: trace.parentSpanId ?? '',
    }));
    const payload = Buffer.from(JSON.stringify({
      type: operation.messageType,
      version: 1,
      requestSpanId,
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
      const fail = (error: Error) => {
        this.pending.delete(requestSpanId);
        clearTimeout(timer);
        signal?.removeEventListener('abort', abort);
        reject(error);
      };
      const abort = () => fail(new Error('AgentVault MQTT request cancelled'));
      const timer = setTimeout(() => fail(new Error('AgentVault MQTT request timed out')), this.config.timeoutMs);
      this.pending.set(requestSpanId, {
        expectedType: operation.responseType,
        commandTopic: `${this.prefix}/commands/${operation.command}`,
        payload,
        resolve,
        reject,
        timer,
        release: () => signal?.removeEventListener('abort', abort),
      });
      signal?.addEventListener('abort', abort, { once: true });
      if (!this.client) {
        try { this.open(); } catch { fail(new Error('AgentVault MQTT connection could not be started')); }
      }
    });
    this.publishPending(requestSpanId);
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
        detail?.code ?? 'TRANSPORT_ERROR',
        detail?.message ?? `AgentVault returned status ${response.status}`,
      );
    }
    return value as T;
  }

  private publishPending(requestSpanId: string): void {
    const client = this.client;
    const waiting = this.pending.get(requestSpanId);
    if (!client || !this.ready || !waiting) return;
    client.publish(waiting.commandTopic, waiting.payload, { qos: 1, retain: false }, () => {
      // A callback error is transient while mqtt.js reconnects. The exact
      // envelope is replayed after the next successful QoS 1 subscription.
    });
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
    if (!value || typeof value !== 'object' || Array.isArray(value)) return;
    const requestSpanId = value.requestSpanId;
    const waiting = typeof requestSpanId === 'string' ? this.pending.get(requestSpanId) : undefined;
    const status = value.status;
    const responseTypeMatches = typeof status === 'number' &&
      ((status >= 400 && value.type === 'payment.error') ||
        (status < 400 && value.type === waiting?.expectedType));
    const headers = value.headers;
    if (!waiting || !responseTypeMatches || value.version !== 1 || typeof requestSpanId !== 'string' ||
        !REQUEST_SPAN_ID.test(requestSpanId) || topic !== `${this.prefix}/results/${requestSpanId}` ||
        !Number.isInteger(status) || (status as number) < 100 || (status as number) > 599 ||
        typeof value.body !== 'string' || !headers || typeof headers !== 'object' || Array.isArray(headers) ||
        Object.values(headers).some((item) => typeof item !== 'string') ||
        !['type', 'version', 'requestSpanId', 'status', 'headers', 'body'].every((key) => key in value) ||
        Object.keys(value).length !== 6) {
      return;
    }
    const response = value as unknown as WireResponse;
    clearTimeout(waiting.timer);
    waiting.release();
    this.pending.delete(requestSpanId);
    waiting.resolve(response);
  }
}

function subscriptionGranted(grants: ReadonlyArray<{ qos: number }> | undefined, expectedCount: number): boolean {
  return grants?.length === expectedCount && grants.every((grant) => grant.qos === 1);
}

function producerTraceContext(headers: Readonly<Record<string, string>>): {
  traceId: string;
  spanId: string;
  parentSpanId?: string;
  traceparent: string;
} {
  const parentValue = Object.entries(headers).find(([name]) => name.toLowerCase() === 'traceparent')?.[1] ?? '';
  const parent = TRACEPARENT.exec(parentValue);
  const traceId = parent?.[1] ?? randomNonzeroHex(16);
  const spanId = randomNonzeroHex(8);
  const flags = parent?.[3] ?? '01';
  return {
    traceId,
    spanId,
    ...(parent?.[2] ? { parentSpanId: parent[2] } : {}),
    traceparent: `00-${traceId}-${spanId}-${flags}`,
  };
}

function randomNonzeroHex(bytes: number): string {
  for (;;) {
    const value = randomBytes(bytes).toString('hex');
    if (!/^0+$/.test(value)) return value;
  }
}
