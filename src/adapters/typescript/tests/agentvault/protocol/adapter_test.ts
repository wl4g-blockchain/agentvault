import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import test from 'node:test';
import { encodePaymentResponseHeader } from '@x402/core/http';
import { Receipt } from 'mppx';
import { PaymentProtocolFactory, type PaymentProtocol } from '../../../src/agentvault/protocol/adapter.js';
import { X402Adapter } from '../../../src/agentvault/protocol/x402.js';

test('shared protocol HTTP fixture covers both transports, protocols, and roles', () => {
  const fixture = JSON.parse(readFileSync(
    new URL('../../../../fixtures/payment_protocol_http.json', import.meta.url), 'utf8',
  )) as {
    schema: string;
    protocols: Record<string, { evidence_header: string }>;
    matrix: Array<Record<string, string>>;
  };
  assert.equal(fixture.schema, 'agentvault.sdk.protocol-http.v1');
  const registry = new PaymentProtocolFactory().create(['x402', 'mpp']);
  const evidenceValues: Record<string, string> = {
    x402: encodePaymentResponseHeader({
      success: true,
      payer: `0x${'33'.repeat(20)}`,
      transaction: '0x1111',
      network: 'eip155:42431',
    }),
    mpp: Receipt.serialize(Receipt.from({
      status: 'success',
      method: 'tempo',
      timestamp: '2033-05-18T03:33:20Z',
      reference: '0x1111',
    })),
  };
  for (const [name, contract] of Object.entries(fixture.protocols)) {
    const evidence = registry.get(name).merchantEvidence(new Headers({
      [contract.evidence_header]: evidenceValues[name]!,
    }));
    assert.equal(evidence?.headerName, contract.evidence_header);
  }
  const combinedEvidence = new Headers();
  for (const [name, contract] of Object.entries(fixture.protocols)) {
    combinedEvidence.set(contract.evidence_header, evidenceValues[name]!);
  }
  assert.throws(() => registry.merchantEvidence(combinedEvidence), /multiple/);
  assert.deepEqual(registry.credentialHeaderNames(), [
    'payment-signature', 'payment-authorization', 'authorization',
  ]);
  assert.deepEqual(registry.merchantEvidenceHeaderNames(), [
    'payment-response', 'payment-receipt',
  ]);
  const expected = [
    'grpc/x402/payer', 'grpc/x402/payee', 'grpc/mpp/payer', 'grpc/mpp/payee',
    'mqtt/x402/payer', 'mqtt/x402/payee', 'mqtt/mpp/payer', 'mqtt/mpp/payee',
  ].sort();
  const actual = new Set(fixture.matrix.map((row) =>
    `${row.transport}/${row.protocol}/${row.role}`));
  assert.equal(fixture.matrix.length, 8);
  assert.deepEqual([...actual].sort(), expected);
});

test('shared protocol MCP fixture uses native provider models', () => {
  const fixture = JSON.parse(readFileSync(
    new URL('../../../../fixtures/payment_protocol_mcp.json', import.meta.url), 'utf8',
  )) as {
    schema: string;
    protocols: Record<string, {
      challenge_response: unknown;
      credential_meta: unknown;
      receipt_meta: unknown;
    }>;
    matrix: Array<Record<string, string>>;
  };
  assert.equal(fixture.schema, 'agentvault.sdk.protocol-mcp.v1');
  const registry = new PaymentProtocolFactory().create(['x402', 'mpp']);
  const combinedCredentials: Record<string, unknown> = {};
  const combinedReceipts: Record<string, unknown> = {};
  for (const [name, contract] of Object.entries(fixture.protocols)) {
    const challenge = registry.mcpChallenge(contract.challenge_response, 16_384);
    assert.equal(challenge?.adapter.name, name);
    const [credentialProtocol, credential] = registry.presentedMcpCredential(
      contract.credential_meta,
      16_384,
    );
    assert.equal(credentialProtocol, name);
    assert.ok(credential);
    const [receiptProtocol, receipt] = registry.mcpMerchantEvidence(
      contract.receipt_meta,
      16_384,
    );
    assert.equal(receiptProtocol, name);
    assert.ok(receipt);
    Object.assign(combinedCredentials, contract.credential_meta);
    Object.assign(combinedReceipts, contract.receipt_meta);
  }
  assert.throws(
    () => registry.presentedMcpCredential(combinedCredentials, 16_384),
    /multiple/,
  );
  assert.throws(
    () => registry.mcpMerchantEvidence(combinedReceipts, 16_384),
    /multiple/,
  );
  assert.deepEqual(registry.mcpRequestMetadataKeys(), [
    'x402/payment', 'org.paymentauth/credential',
  ]);
  assert.deepEqual(registry.mcpRootRequestMetadataKeys(), [
    'org.paymentauth/credential',
  ]);
  assert.deepEqual(registry.mcpReceiptMetadataKeys(), [
    'x402/payment-response', 'org.paymentauth/receipt',
  ]);
  const expected = [
    'grpc/x402/payer', 'grpc/x402/payee', 'grpc/mpp/payer', 'grpc/mpp/payee',
    'mqtt/x402/payer', 'mqtt/x402/payee', 'mqtt/mpp/payer', 'mqtt/mpp/payee',
  ].sort();
  assert.deepEqual(fixture.matrix.map((row) =>
    `${row.transport}/${row.protocol}/${row.role}`).sort(), expected);
});

test('protocol factory instances are isolated and reject duplicate providers', () => {
  const first = new PaymentProtocolFactory();
  first.register('test', () => ({ ...new X402Adapter(), name: 'test' }) as PaymentProtocol);
  assert.equal(first.create(['test']).get('test').name, 'test');

  const second = new PaymentProtocolFactory();
  assert.throws(() => second.create(['test']), /unavailable/);
  assert.throws(() => second.create(['x402', 'x402']), /duplicate/);
});
