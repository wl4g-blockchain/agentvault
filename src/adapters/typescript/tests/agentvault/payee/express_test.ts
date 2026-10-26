import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import type { AddressInfo } from 'node:net';
import test from 'node:test';
import express from 'express';
import { Challenge, Mcp } from 'mppx';
import type { VaultTransport } from '../../../src/agentvault/transport/transport.js';
import { PayeeMiddleware } from '../../../src/agentvault/payee/payee.js';
import {
  mcpToolRoute,
  mcpToolRouteWithProtocol,
} from '../../../src/agentvault/payee/express.js';

test('ExpressPayeeMiddleware exposes raw-body and protection handlers', () => {
  const transport: VaultTransport = { async request<T>(): Promise<T> { throw new Error('not called'); } };
  const middleware = new PayeeMiddleware(transport).express([
    mcpToolRoute('/mcp', 'marketpulse.realtime_news', 'scenario-1', 'price-1',
      'mcp://marketpulse/realtime', 'https://marketpulse.test'),
  ]);
  assert.equal(middleware.handlers().length, 2);
});

test('ExpressPayeeMiddleware bypasses a free tool on the shared MCP route', async () => {
  const transport: VaultTransport = { async request<T>(): Promise<T> { throw new Error('Vault must not be called'); } };
  const middleware = new PayeeMiddleware(transport).express([
    mcpToolRoute('/mcp', 'marketpulse.realtime_news', 'scenario-1', 'price-1',
      'mcp://marketpulse/realtime', 'https://marketpulse.test'),
  ]);
  let passed = false;
  const request = {
    method: 'POST', path: '/mcp', headers: {},
    body: Buffer.from(JSON.stringify({ method: 'tools/call', params: { name: 'marketpulse.yesterday_news' } })),
  };
  await middleware.handler()(request as never, {} as never, () => { passed = true; });
  assert.equal(passed, true);
});

test('ExpressPayeeMiddleware rejects duplicated payment credentials before Vault', async () => {
  const transport: VaultTransport = { async request<T>(): Promise<T> { throw new Error('Vault must not be called'); } };
  const middleware = new PayeeMiddleware(transport).express([
    mcpToolRoute('/mcp', 'marketpulse.realtime_news', 'scenario-1', 'price-1',
      'mcp://marketpulse/realtime', 'https://marketpulse.test'),
  ]);
  const request = {
    method: 'POST', path: '/mcp',
    headers: { 'x-run-id': 'run-1', 'payment-signature': 'e30=' },
    rawHeaders: ['x-run-id', 'run-1', 'payment-signature', 'e30=', 'payment-signature', 'e30='],
    body: Buffer.from(JSON.stringify({
      jsonrpc: '2.0', id: 1, method: 'tools/call', params: { name: 'marketpulse.realtime_news' },
    })),
  };
  let status = 0;
  let value: unknown;
  const response = {
    locals: {},
    status(code: number) { status = code; return this; },
    json(body: unknown) { value = body; return this; },
  };

  await middleware.handler()(request as never, response as never, () => {
    throw new Error('ambiguous credential reached the protected application');
  });

  assert.equal(status, 400);
  assert.match(JSON.stringify(value), /duplicate payment request header/);
});

test('ExpressPayeeMiddleware closes x402 and MPP MCP verify/settle flows', async () => {
  const fixture = JSON.parse(readFileSync(
    new URL('../../../../tests/payment_protocol_mcp.json', import.meta.url),
    'utf8',
  )) as {
    protocols: Record<string, {
      challenge_response: Record<string, unknown>;
      credential_meta: Record<string, unknown>;
    }>;
  };

  for (const protocol of ['x402', 'mpp'] as const) {
    const contract = fixture.protocols[protocol]!;
    const transport = new McpPayeeTransport(protocol, contract);
    const applicationAuthorizations: Array<string | undefined> = [];
    const applicationBodies: Array<Record<string, unknown>> = [];
    const app = express();
    const middleware = new PayeeMiddleware(transport, { protocols: [protocol] }).express([
      mcpToolRouteWithProtocol(
        '/mcp',
        'marketpulse.realtime',
        'scenario',
        'price',
        'mcp://marketpulse/realtime',
        'https://merchant.test',
        protocol,
      ),
    ]);
    app.post('/mcp', ...middleware.handlers(), (request, response) => {
      applicationAuthorizations.push(request.headers.authorization);
      applicationBodies.push(JSON.parse((request.body as Buffer).toString('utf8')) as Record<string, unknown>);
      response.json({
        jsonrpc: '2.0',
        id: 1,
        result: { content: [{ type: 'text', text: 'paid' }] },
      });
    });
    const server = app.listen(0, '127.0.0.1');
    await new Promise<void>((resolve) => server.once('listening', resolve));
    const address = server.address() as AddressInfo;
    const endpoint = `http://127.0.0.1:${address.port}/mcp`;
    const request: {
      jsonrpc: string;
      id: number;
      method: string;
      params: Record<string, unknown>;
      _meta?: Record<string, unknown>;
    } = {
      jsonrpc: '2.0',
      id: 1,
      method: 'tools/call',
      params: {
        name: 'marketpulse.realtime',
        arguments: { symbol: 'AVT' },
      } as Record<string, unknown>,
    };
    if (protocol === 'mpp') request._meta = { 'application/root': 'preserved' };
    const headers = {
      authorization: 'Bearer application-token',
      'content-type': 'application/json',
      'x-run-id': 'run-1',
    };
    try {
      const unpaid = await fetch(endpoint, {
        method: 'POST', headers, body: JSON.stringify(request),
      });
      assert.equal(unpaid.status, 200);
      assert.equal(unpaid.headers.get('payment-protocol'), protocol);
      assert.equal(unpaid.headers.get('payment-expires-at'), '2000000060');
      const challenge = await unpaid.json() as {
        result?: { isError?: boolean };
        error?: { code?: number };
      };
      if (protocol === 'x402') assert.equal(challenge.result?.isError, true);
      else assert.equal(challenge.error?.code, Mcp.paymentRequiredCode);
      assert.equal(applicationAuthorizations.length, 0);

      if (protocol === 'mpp') {
        request._meta = { ...request._meta, ...contract.credential_meta };
      } else {
        request.params._meta = contract.credential_meta;
      }
      const paid = await fetch(endpoint, {
        method: 'POST', headers, body: JSON.stringify(request),
      });
      assert.equal(paid.status, 200);
      assert.equal(paid.headers.has('payment-response'), false);
      assert.equal(paid.headers.has('payment-receipt'), false);
      const result = await paid.json() as {
        result: { _meta: Record<string, unknown> };
      };
      const receiptKey = protocol === 'x402' ? 'x402/payment-response' : Mcp.receiptMetaKey;
      assert.ok(result.result._meta[receiptKey]);
      assert.deepEqual(applicationAuthorizations, ['Bearer application-token']);
      const forwardedParams = applicationBodies[0]?.params as Record<string, unknown>;
      const forwardedMeta = (forwardedParams._meta ?? {}) as Record<string, unknown>;
      assert.equal('x402/payment' in forwardedMeta, false);
      assert.equal('org.paymentauth/credential' in forwardedMeta, false);
      const forwardedRootMeta = (applicationBodies[0]?._meta ?? {}) as Record<string, unknown>;
      assert.equal('org.paymentauth/credential' in forwardedRootMeta, false);
      if (protocol === 'mpp') assert.equal(forwardedRootMeta['application/root'], 'preserved');
      assert.equal(transport.requestHashes[0], transport.requestHashes[1]);
      if (protocol === 'mpp') {
        request.params._meta = contract.credential_meta;
        const ambiguous = await fetch(endpoint, {
          method: 'POST', headers, body: JSON.stringify(request),
        });
        assert.equal(ambiguous.status, 400);
        assert.match(await ambiguous.text(), /duplicate MCP payment credential metadata/);
        assert.equal(applicationBodies.length, 1);
      }
      assert.deepEqual(transport.paths, [
        '/api/v1/payee/challenges',
        '/api/v1/payee/challenges',
        '/api/v1/payee/verify',
        '/api/v1/payee/settle',
      ]);
    } finally {
      await new Promise<void>((resolve, reject) => server.close((error) =>
        error ? reject(error) : resolve()));
    }
  }
});

test('ExpressPayeeMiddleware maps rejected MPP MCP credentials to -32043', async () => {
  const fixture = JSON.parse(readFileSync(
    new URL('../../../../tests/payment_protocol_mcp.json', import.meta.url),
    'utf8',
  )) as {
    protocols: Record<string, {
      challenge_response: Record<string, unknown>;
      credential_meta: Record<string, unknown>;
    }>;
  };
  const contract = fixture.protocols.mpp!;
  const transport = new McpPayeeTransport('mpp', contract);
  transport.reject = true;
  const app = express();
  const middleware = new PayeeMiddleware(transport, { protocols: ['mpp'] }).express([
    mcpToolRouteWithProtocol(
      '/mcp', 'marketpulse.realtime', 'scenario', 'price',
      'mcp://marketpulse/realtime', 'https://merchant.test', 'mpp',
    ),
  ]);
  app.post('/mcp', ...middleware.handlers(), (_request, _response) => {
    throw new Error('invalid payment released the protected tool');
  });
  const server = app.listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', resolve));
  const address = server.address() as AddressInfo;
  try {
    const response = await fetch(`http://127.0.0.1:${address.port}/mcp`, {
      method: 'POST',
      headers: {
        authorization: 'Bearer application-token',
        'content-type': 'application/json',
        'x-run-id': 'run-1',
      },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: 1,
        method: 'tools/call',
        params: {
          name: 'marketpulse.realtime',
          arguments: { symbol: 'AVT' },
          _meta: contract.credential_meta,
        },
      }),
    });
    assert.equal(response.status, 200);
    const message = await response.json() as {
      error: { code: number; data: { challenges: unknown[] } };
    };
    assert.equal(message.error.code, Mcp.paymentVerificationFailedCode);
    assert.ok(message.error.data.challenges.length > 0);
    assert.deepEqual(transport.paths, [
      '/api/v1/payee/challenges',
      '/api/v1/payee/verify',
    ]);
  } finally {
    await new Promise<void>((resolve, reject) => server.close((error) =>
      error ? reject(error) : resolve()));
  }
});

test('ExpressPayeeMiddleware preserves a paid MCP business failure for delivery retry', async () => {
  const fixture = JSON.parse(readFileSync(
    new URL('../../../../tests/payment_protocol_mcp.json', import.meta.url),
    'utf8',
  )) as {
    protocols: Record<string, {
      challenge_response: Record<string, unknown>;
      credential_meta: Record<string, unknown>;
    }>;
  };
  const contract = fixture.protocols.x402!;
  const app = express();
  const middleware = new PayeeMiddleware(
    new McpPayeeTransport('x402', contract),
    { protocols: ['x402'] },
  ).express([
    mcpToolRoute(
      '/mcp', 'marketpulse.realtime', 'scenario', 'price',
      'mcp://marketpulse/realtime', 'https://merchant.test',
    ),
  ]);
  app.post('/mcp', ...middleware.handlers(), (_request, response) => {
    response.status(503).json({
      jsonrpc: '2.0',
      id: 1,
      error: { code: -32004, message: 'retry delivery' },
    });
  });
  const server = app.listen(0, '127.0.0.1');
  await new Promise<void>((resolve) => server.once('listening', resolve));
  const address = server.address() as AddressInfo;
  try {
    const response = await fetch(`http://127.0.0.1:${address.port}/mcp`, {
      method: 'POST',
      headers: { 'content-type': 'application/json', 'x-run-id': 'run-1' },
      body: JSON.stringify({
        jsonrpc: '2.0',
        id: 1,
        method: 'tools/call',
        params: { name: 'marketpulse.realtime', _meta: contract.credential_meta },
      }),
    });
    assert.equal(response.status, 503);
    const message = await response.json() as { error: { message: string }; result?: unknown };
    assert.equal(message.error.message, 'retry delivery');
    assert.equal(message.result, undefined);
  } finally {
    await new Promise<void>((resolve, reject) => server.close((error) =>
      error ? reject(error) : resolve()));
  }
});

class McpPayeeTransport implements VaultTransport {
  readonly paths: string[] = [];
  readonly requestHashes: string[] = [];
  reject = false;

  constructor(
    private readonly protocol: 'x402' | 'mpp',
    private readonly contract: {
      challenge_response: Record<string, unknown>;
      credential_meta: Record<string, unknown>;
    },
  ) {}

  async request<T>(_method: 'GET' | 'POST', path: string, body?: unknown): Promise<T> {
    this.paths.push(path);
    const wire = body as Record<string, unknown>;
    if (path === '/api/v1/payee/challenges') {
      this.requestHashes.push(wire.request_hash as string);
      return this.challenge() as T;
    }
    if (path === '/api/v1/payee/verify') {
      const intent = this.intent(wire.request_hash as string, 'pending_settlement', '0');
      return {
        intent,
        valid: !this.reject,
        ...(!this.reject && { verification_ref: 'verification-1' }),
      } as T;
    }
    if (path === '/api/v1/payee/settle') {
      return {
        intent: this.intent(this.requestHashes.at(-1)!, 'settled', '1000'),
        receipt: {
          intent_id: 'intent-1',
          scenario_id: 'scenario',
          direction: 'inbound',
          protocol: this.protocol,
          asset: this.asset(),
          recipient: `0x${'11'.repeat(20)}`,
          payer: `0x${'33'.repeat(20)}`,
          requested_amount: '1000',
          settled_amount: '1000',
          status: 'settled',
          tx_hash: '0x1111',
          settlement_ref: 'settlement-1',
          evidence_hash: 'evidence-1',
          confirmed_at: 2_000_000_000,
        },
      } as T;
    }
    throw new Error(`unexpected AgentVault path: ${path}`);
  }

  private challenge() {
    if (this.protocol === 'x402') {
      const response = this.contract.challenge_response as {
        result: { structuredContent: unknown };
      };
      return {
        ...this.challengeBase('nonce-1'),
        payload: { payment_required: response.result.structuredContent },
      };
    }
    const response = this.contract.challenge_response as {
      error: { data: { challenges: Challenge.Challenge[] } };
    };
    const challenge = Challenge.Schema.parse(response.error.data.challenges[0]);
    return {
      ...this.challengeBase(challenge.id),
      payload: {
        mcp_challenge: challenge,
        www_authenticate: Challenge.serialize(challenge),
      },
    };
  }

  private challengeBase(nonce: string) {
    return {
      scenario_id: 'scenario',
      price_id: 'price',
      resource: 'mcp://marketpulse/realtime',
      protocol: this.protocol,
      asset: this.asset(),
      amount: '1000',
      recipient: `0x${'11'.repeat(20)}`,
      nonce,
      challenge_hash: 'challenge-1',
      expires_at: 2_000_000_060,
    };
  }

  private intent(requestHash: string, status: 'pending_settlement' | 'settled', settled: string) {
    return {
      id: 'intent-1',
      scenario_id: 'scenario',
      run_id: 'run-1',
      price_id: 'price',
      direction: 'inbound',
      protocol: this.protocol,
      asset: this.asset(),
      recipient: `0x${'11'.repeat(20)}`,
      amount: '1000',
      settled_amount: settled,
      resource: 'mcp://marketpulse/realtime',
      challenge_hash: 'challenge-1',
      request_hash: requestHash,
      status,
      reservation_status: 'unreserved',
      reserved_amount: '0',
      created_at: 1,
      updated_at: 2,
    };
  }

  private asset() {
    return {
      network: 'eip155:42431',
      asset: 'USDC',
      decimals: 6,
      token_contract: `0x${'22'.repeat(20)}`,
    };
  }
}
