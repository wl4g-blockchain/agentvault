import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { StreamableHTTPClientTransport } from '@modelcontextprotocol/sdk/client/streamableHttp.js';
import {
  AgentVault,
  GrpcConfig,
  MqttConfig,
  PayerRequestContext,
  readBounded,
  type FetchPayerMiddleware,
  type VaultTransportConfig,
} from '@agentvault/sdk';

const YESTERDAY_TOOL = 'marketpulse.yesterday_news';
const REALTIME_TOOL = 'marketpulse.realtime_news';

export interface ToolResult { body: Uint8Array; paymentIntentId?: string }

interface PaymentMetadata {
  runId: string;
  paid: boolean;
  failAfterSettlement: boolean;
}

/** MCPHelper owns one official MCP client and its two discovered remote tools. */
export class MCPHelper {
  readonly tools: ReadonlySet<string>;

  private constructor(
    private readonly client: Client,
    private readonly vault: AgentVault,
    private readonly payments: Map<string, string>,
    tools: Iterable<string>,
  ) {
    this.tools = new Set(tools);
  }

  static async fromEnvironment(): Promise<MCPHelper> {
    const timeout = positive('AGENTVAULT_MQTT_TIMEOUT_SECONDS') * 1000;
    const transportName = (process.env.AGENTVAULT_TRANSPORT ?? 'mqtt').trim().toLowerCase();
    let transportConfig: VaultTransportConfig;
    if (transportName === 'mqtt') {
      transportConfig = { provider: 'mqtt', mqtt: new MqttConfig(
        required('AGENTVAULT_MQTT_HOST'), positive('AGENTVAULT_MQTT_PORT'),
        boolean('AGENTVAULT_MQTT_TLS'), required('AGENTVAULT_MQTT_USERNAME'),
        required('AGENTVAULT_MQTT_PASSWORD'), 'quantstream-typescript',
        required('QUANTSTREAM_CLIENT_ID'), timeout,
      ) };
    } else if (transportName === 'grpc') {
      transportConfig = { provider: 'grpc', grpc: new GrpcConfig(
        required('AGENTVAULT_GRPC_ENDPOINT'), boolean('AGENTVAULT_GRPC_TLS'), undefined, timeout,
      ) };
    } else {
      throw new Error('AGENTVAULT_TRANSPORT must be mqtt or grpc');
    }
    const vault = AgentVault.fromConfig(transportConfig);
    console.info(JSON.stringify({
      event: 'agentvault.sdk.payer.transport_selected', transport: transportName,
    }));
    const payer = vault.payer({
      scenarioId: required('QUANTSTREAM_SCENARIO_ID'),
      approvalTimeoutMs: positive('AGENTVAULT_APPROVAL_TIMEOUT_SECONDS') * 1000,
      deliveryRetries: 1,
    }).standard();
    const payments = new Map<string, string>();
    const paidFetch = mcpFetch(payer, payments);
    const mcpTransport = new StreamableHTTPClientTransport(
      new URL(required('MARKETPULSE_URL')),
      { fetch: paidFetch },
    );
    const client = new Client({ name: 'quantstream-typescript', version: '0.1.0' });
    try {
      await client.connect(mcpTransport);
      const listed = await client.listTools();
      const tools = listed.tools.map((tool) => tool.name);
      for (const requiredTool of [YESTERDAY_TOOL, REALTIME_TOOL]) {
        if (!tools.includes(requiredTool)) throw new Error(`MarketPulse MCP server did not expose ${requiredTool}`);
      }
      return new MCPHelper(client, vault, payments, tools);
    } catch (error: unknown) {
      await client.close().catch(() => undefined);
      await vault.close();
      throw error;
    }
  }

  async close(): Promise<void> {
    await this.client.close();
    await this.vault.close();
  }

  yesterdayNews(runId: string): Promise<ToolResult> {
    return this.call(YESTERDAY_TOOL, runId, false);
  }

  realtimeNews(runId: string, failAfterSettlement = false): Promise<ToolResult> {
    return this.call(REALTIME_TOOL, runId, failAfterSettlement);
  }

  private async call(
    tool: string,
    runId: string,
    failAfterSettlement: boolean,
  ): Promise<ToolResult> {
    if (!this.tools.has(tool)) throw new Error(`MarketPulse MCP tool ${JSON.stringify(tool)} is not registered`);
    const metadata: PaymentMetadata = { runId, paid: tool === REALTIME_TOOL, failAfterSettlement };
    const result = await this.client.callTool({
      name: tool,
      arguments: {},
      _meta: { agentvault: metadata },
    });
    if (result.isError) throw new Error(`MarketPulse MCP tool ${tool} returned an application error`);
    const body = new TextEncoder().encode(JSON.stringify(result.structuredContent ?? {}));
    const paymentIntentId = this.payments.get(runId);
    this.payments.delete(runId);
    if (tool === REALTIME_TOOL && !paymentIntentId) {
      throw new Error('paid MCP result omitted AgentVault payment metadata');
    }
    return paymentIntentId ? { body, paymentIntentId } : { body };
  }
}

function mcpFetch(payer: FetchPayerMiddleware, payments: Map<string, string>): typeof globalThis.fetch {
  return async (input: URL | RequestInfo, init?: RequestInit): Promise<Response> => {
    const source = new Request(input, init);
    const metadata = await paymentMetadata(source.clone());
    if (!metadata) return payer.request(source);
    const headers = new Headers(source.headers);
    headers.set('x-run-id', metadata.runId);
    if (metadata.failAfterSettlement) headers.set('x-e2e-fail-after-settlement', 'once');
    const operation = metadata.paid ? new PayerRequestContext(metadata.runId) : undefined;
    const response = await payer.request(new Request(source, { headers }), undefined, operation);
    const payment = payer.payment(response);
    if (payment) payments.set(metadata.runId, payment.intent.id);
    return response;
  };
}

async function paymentMetadata(request: Request): Promise<PaymentMetadata | undefined> {
  if (request.method !== 'POST') return undefined;
  const body = await readBounded(await fetchlessResponse(request), 1024 * 1024);
  let value: unknown;
  try { value = JSON.parse(new TextDecoder().decode(body)); } catch { return undefined; }
  if (!isRecord(value) || value.method !== 'tools/call' || !isRecord(value.params)) return undefined;
  const meta = value.params._meta;
  if (!isRecord(meta) || !isRecord(meta.agentvault)) return undefined;
  const candidate = meta.agentvault;
  if (typeof candidate.runId !== 'string' || typeof candidate.paid !== 'boolean'
      || typeof candidate.failAfterSettlement !== 'boolean') return undefined;
  return candidate as unknown as PaymentMetadata;
}

async function fetchlessResponse(request: Request): Promise<Response> {
  return new Response(await request.arrayBuffer(), { headers: request.headers });
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null;
}

export function required(name: string): string {
  const value = process.env[name]?.trim();
  if (!value) throw new Error(`${name} is required`);
  return value;
}

export function positive(name: string): number {
  const value = Number(required(name));
  if (!Number.isInteger(value) || value <= 0) throw new Error(`${name} must be positive`);
  return value;
}

function boolean(name: string): boolean {
  const value = required(name).toLowerCase();
  if (!['true', 'false'].includes(value)) throw new Error(`${name} must be boolean`);
  return value === 'true';
}
