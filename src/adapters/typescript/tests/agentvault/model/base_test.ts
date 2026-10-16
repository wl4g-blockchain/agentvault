import assert from 'node:assert/strict';
import { inspect } from 'node:util';
import test from 'node:test';
import { assertIdempotencyKey, PaymentCredential } from '../../../src/agentvault/model/base.js';

test('base models enforce idempotency and redact credentials', () => {
  assert.doesNotThrow(() => assertIdempotencyKey('run-1:realtime'));
  assert.throws(() => assertIdempotencyKey('unsafe/key'));
  const credential = new PaymentCredential(
    'x402', 'challenge-secret', 'credential-secret', { signature: 'signature-secret' },
    { bound_origin: 'https://merchant.test', header_name: 'PAYMENT-SIGNATURE', header_value: 'header-secret' },
  );
  const rendered = inspect(credential);
  for (const secret of ['challenge-secret', 'credential-secret', 'signature-secret', 'header-secret']) {
    assert.doesNotMatch(rendered, new RegExp(secret));
  }
});
