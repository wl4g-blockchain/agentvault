import assert from 'node:assert/strict';
import test from 'node:test';
import {readFileSync} from 'node:fs';
import type {AddressInfo} from 'node:net';
import express from 'express';
import {InMemoryTaskStore} from '@a2a-js/sdk/server';
import {Challenge, Credential, Mcp} from 'mppx';
import {encodePaymentSignatureHeader} from '@x402/core/http';
import type {PaymentPayload} from '@x402/core/types';
import type {VaultTransport} from '../../../src/agentvault/transport/transport.js';
import {PayeeMiddleware} from '../../../src/agentvault/payee/payee.js';
import {PayerMiddleware} from '../../../src/agentvault/payer/payer.js';
import {PayerRequestContext} from '../../../src/agentvault/model/payer.js';

test('A2A both roles, both protocols retain native Task and business failure receipt', async () => {
  const carrier = JSON.parse(readFileSync(new URL('../../../../tests/payment_protocol_a2a.json', import.meta.url), 'utf8'));
  const fixture = JSON.parse(readFileSync(new URL('../../../../tests/payment_protocol_mcp.json', import.meta.url), 'utf8'));
  for (const protocol of ['x402', 'mpp'] as const) for (const failed of [false, true]) {
    const vault = new A2ATestVault(protocol, fixture.protocols[protocol]);
    const store = new InMemoryTaskStore();
    await store.save({kind:'task', id:'task-1', contextId:'context-1', status:{state:'submitted'}});
    const app = express(); let executions = 0;
    const middleware = new PayeeMiddleware(vault, {protocols:[protocol]}).express([{
      method:'POST', path:'/a2a', scenarioId:'scenario', priceId:'price', resource:'mcp://marketpulse/realtime',
      origin:'https://merchant.test', protocol, a2aTaskStore:store,
    }]);
    app.post('/a2a', ...middleware.handlers(), (req, res) => {
      executions++;
      assert.equal(req.headers.authorization, 'Bearer application-token');
      const call = JSON.parse(req.body.toString());
      assert.equal(call.params.message.taskId, 'task-1');
      assert.deepEqual(call.params.message.metadata, {'application.label':'keep'});
      res.json(failed ? {jsonrpc:'2.0', id:call.id, error:{code:-32603, message:'business failed'}} : {
        jsonrpc:'2.0', id:call.id, result:{kind:'task', id:'task-1', contextId:'context-1', status:{state:'completed'}},
      });
    });
    const server = app.listen(0, '127.0.0.1');
    await new Promise<void>((resolve) => server.once('listening', resolve));
    vault.origin = `http://127.0.0.1:${(server.address() as AddressInfo).port}`;
    try {
      const payer = new PayerMiddleware(vault, {scenarioId:'scenario', protocols:[protocol]}).standard();
      const response = await payer.request(vault.origin + '/a2a', {method:'POST', headers:{authorization:'Bearer application-token', 'content-type':'application/json'},
        body:JSON.stringify(carrier.request)}, new PayerRequestContext('run-1'));
      const result = await response.json() as any;
      assert.equal(response.status, 200, JSON.stringify(result));
      assert.equal(result.result.status.state, failed ? 'failed' : 'completed');
      assert.equal(executions, 1);
      assert.equal(new Set(vault.requestHashes).size, 1);
      assert.equal(vault.paths.filter(path => path.endsWith('outbound-response')).length, 1);
      assert.equal((await store.load('task-1'))?.metadata?.['agentvault.payment.run_id'], 'run-1');
    } finally { await new Promise<void>((resolve, reject) => server.close(error => error ? reject(error) : resolve())); }
  }
});
class A2ATestVault implements VaultTransport {
  readonly paths: string[] = [];
  readonly requestHashes: string[] = [];
  reject = false;
  origin = '';
  outbound(status: string) { return {...this.intent('request-1', 'pending_settlement', '0'), id:'out-1', direction:'outbound',
    wallet_id:'wallet', policy_id:'policy', resource:this.origin + '/a2a', status, reservation_status:'reserved', reserved_amount:'1000'}; }

  constructor(
    readonly protocol: 'x402' | 'mpp',
    readonly contract: {
      challenge_response: Record<string, unknown>;
      credential_meta: Record<string, unknown>;
    },
  ) {}

  async request<T>(_method: 'GET' | 'POST', path: string, body?: unknown): Promise<T> {
    this.paths.push(path);
    const wire = body as Record<string, unknown>;
    if (path === '/api/v1/payments/authorize') {
      const payload = this.contract.credential_meta[this.protocol === 'x402' ? 'x402/payment' : Mcp.credentialMetaKey];
      return {authorization:{outcome:'authorized', intent_id:'out-1'}, intent: this.outbound('signed'),
        credential:{protocol:this.protocol, challenge_hash:'challenge-1', credential_hash:'credential-1', payload,
          http:{bound_origin:this.origin, header_name:this.protocol === 'x402' ? 'Payment-Signature' : 'Payment-Authorization',
            header_value:this.protocol === 'x402' ? encodePaymentSignatureHeader(payload as PaymentPayload) : Credential.serialize(payload as Credential.Credential)}}} as T;
    }
    if (path === '/api/v1/payments/out-1/outbound-response') return {intent:this.outbound('pending_settlement'), accepted:true, replayed:false} as T;
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
