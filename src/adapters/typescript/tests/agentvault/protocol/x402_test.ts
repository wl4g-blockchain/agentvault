import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { decodePaymentRequiredHeader } from '@x402/core/http';
import type { PaymentChallenge } from '../../../src/agentvault/model/base.js';
import { X402Adapter } from '../../../src/agentvault/protocol/x402.js';

test('X402Adapter uses only native x402 challenge headers', () => {
  const fixture = JSON.parse(readFileSync(
    new URL('../../../../fixtures/payment_protocol_mcp.json', import.meta.url),
    'utf8',
  )) as { protocols: { x402: { challenge_response: { result: { structuredContent: unknown } } } } };
  const challenge: PaymentChallenge = {
    scenario_id: 'scenario', offer_id: 'offer', resource: 'https://merchant.example/tool', protocol: 'x402',
    asset: { network: 'eip155:42431', asset: 'USDC', decimals: 6 }, amount: '1000',
    recipient: '0x1111111111111111111111111111111111111111', nonce: 'nonce',
    challenge_hash: 'challenge', expires_at: 2_000_000_000,
    payload: { payment_required: fixture.protocols.x402.challenge_response.result.structuredContent },
  };
  const response = new X402Adapter().challenge(challenge);
  const decoded = decodePaymentRequiredHeader(response.headers['payment-required'] ?? '');
  assert.equal(decoded.x402Version, 2);
  assert.equal(decoded.accepts.length, 1);
  assert.equal(response.headers['www-authenticate'], undefined);
});

test('X402Adapter ignores unrelated tool errors and compares MCP challenges structurally', () => {
  const adapter = new X402Adapter();
  assert.equal(adapter.mcpChallenge({
    jsonrpc: '2.0', id: 1,
    result: {
      isError: true,
      structuredContent: { error: 'permission denied' },
      content: [{ type: 'text', text: 'permission denied' }],
    },
  }, 64 * 1024), undefined);

  const required = {
    x402Version: 2,
    resource: { url: 'mcp://tool/test', mimeType: 'application/json' },
    accepts: [{
      scheme: 'exact', network: 'eip155:42431', amount: '1',
      payTo: `0x${'11'.repeat(20)}`, maxTimeoutSeconds: 60,
      asset: `0x${'22'.repeat(20)}`,
    }],
  };
  const reordered = {
    accepts: required.accepts,
    resource: required.resource,
    x402Version: required.x402Version,
  };
  assert.ok(adapter.mcpChallenge({
    result: {
      isError: true,
      structuredContent: required,
      content: [{ type: 'text', text: JSON.stringify(reordered) }],
    },
  }, 64 * 1024));
});
