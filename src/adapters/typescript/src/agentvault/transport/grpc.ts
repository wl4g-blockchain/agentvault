import { randomBytes } from 'node:crypto';
import { inspect } from 'node:util';
import * as grpc from '@grpc/grpc-js';
import { GrpcConfig } from '../config/config.js';
import { ApiError, type VaultTransport } from './transport.js';

const EXECUTE = '/agentvault.transport.v1.PaymentTransport/Execute';

interface WireResponse {
  status: number;
  headers: Record<string, string>;
  body: string;
}

export class GrpcVaultTransport implements VaultTransport {
  private readonly client: grpc.Client;

  constructor(public readonly config: GrpcConfig) {
    const credentials = config.tls ? grpc.credentials.createSsl() : grpc.credentials.createInsecure();
    this.client = new grpc.Client(config.endpoint, credentials, {
      'grpc.max_send_message_length': config.maxMessageBytes,
      'grpc.max_receive_message_length': config.maxMessageBytes,
    });
  }

  [inspect.custom](): string {
    return `GrpcVaultTransport { config: ${inspect(this.config)}, client: <redacted> }`;
  }

  async request<T>(method: 'GET' | 'POST', path: string, body?: unknown): Promise<T> {
    if (!path.startsWith('/api/v1/') || path.includes('\\')) throw new Error('invalid AgentVault gRPC path');
    const headers: Record<string, string> = { accept: 'application/json' };
    const encodedBody = body === undefined ? Buffer.alloc(0) : Buffer.from(JSON.stringify(body));
    if (body !== undefined) headers['content-type'] = 'application/json';
    const envelope = Buffer.from(JSON.stringify({
      method,
      path,
      headers,
      body: encodedBody.toString('base64url'),
    }));
    if (envelope.length > this.config.maxMessageBytes) {
      throw new Error('AgentVault gRPC request exceeds configured limit');
    }
    const metadata = new grpc.Metadata();
    const trace = producerTraceContext();
    metadata.set('traceparent', trace.traceparent);
    if (this.config.authguardBearer) metadata.set('authorization', `Bearer ${this.config.authguardBearer}`);
    console.info(JSON.stringify({
      event: 'agentvault.sdk.transport.grpc.produced',
      traceId: trace.traceId,
      spanId: trace.spanId,
      parentId: '',
    }));
    const deadline = Date.now() + this.config.timeoutMs;
    const raw = await new Promise<Buffer>((resolve, reject) => {
      this.client.makeUnaryRequest(
        EXECUTE,
        encodeBytesValue,
        decodeBytesValue,
        envelope,
        metadata,
        { deadline },
        (error, value) => {
          if (error) reject(new Error(`AgentVault gRPC request failed: ${grpc.status[error.code] ?? error.code}`));
          else if (value) resolve(value);
          else reject(new Error('AgentVault gRPC response is missing'));
        },
      );
    });
    if (raw.length > this.config.maxMessageBytes) {
      throw new Error('AgentVault gRPC response exceeds configured limit');
    }
    let wire: WireResponse;
    let decoded: Buffer;
    let value: unknown;
    try {
      wire = JSON.parse(raw.toString('utf8')) as WireResponse;
      decoded = Buffer.from(wire.body, 'base64url');
      value = JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(decoded));
    } catch {
      throw new Error('invalid AgentVault gRPC response');
    }
    if (!Number.isInteger(wire.status) || wire.status < 100 || wire.status > 599 ||
        decoded.length > this.config.maxMessageBytes || decoded.toString('base64url') !== wire.body ||
        !value || typeof value !== 'object' || Array.isArray(value)) {
      throw new Error('invalid or oversized AgentVault gRPC response');
    }
    if (wire.status < 200 || wire.status >= 300) {
      const detail = (value as { error?: { code?: string; message?: string } }).error;
      throw new ApiError(
        wire.status,
        detail?.code ?? 'TRANSPORT_ERROR',
        detail?.message ?? `AgentVault returned status ${wire.status}`,
      );
    }
    return value as T;
  }

  close(): void {
    this.client.close();
  }
}

function producerTraceContext(): { traceId: string; spanId: string; traceparent: string } {
  const traceId = randomNonzeroHex(16);
  const spanId = randomNonzeroHex(8);
  return { traceId, spanId, traceparent: `00-${traceId}-${spanId}-01` };
}

function randomNonzeroHex(bytes: number): string {
  for (;;) {
    const value = randomBytes(bytes).toString('hex');
    if (!/^0+$/.test(value)) return value;
  }
}

function encodeBytesValue(value: Buffer): Buffer {
  return Buffer.concat([Buffer.from([0x0a]), encodeVarint(value.length), value]);
}

function decodeBytesValue(value: Buffer): Buffer {
  if (value[0] !== 0x0a) throw new Error('invalid AgentVault gRPC envelope');
  const { length, bytes } = decodeVarint(value, 1);
  const start = 1 + bytes;
  if (start + length !== value.length) throw new Error('invalid AgentVault gRPC envelope');
  return value.subarray(start);
}

function encodeVarint(value: number): Buffer {
  const bytes: number[] = [];
  let remaining = value;
  do {
    let byte = remaining & 0x7f;
    remaining = Math.floor(remaining / 128);
    if (remaining > 0) byte |= 0x80;
    bytes.push(byte);
  } while (remaining > 0);
  return Buffer.from(bytes);
}

function decodeVarint(value: Buffer, offset: number): { length: number; bytes: number } {
  let length = 0;
  let shift = 0;
  for (let index = offset; index < value.length && index < offset + 5; index += 1) {
    const byte = value[index]!;
    length += (byte & 0x7f) * (2 ** shift);
    if ((byte & 0x80) === 0) return { length, bytes: index - offset + 1 };
    shift += 7;
  }
  throw new Error('invalid AgentVault gRPC envelope');
}
