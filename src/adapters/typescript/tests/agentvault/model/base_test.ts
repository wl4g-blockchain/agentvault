import assert from 'node:assert/strict';
import { inspect } from 'node:util';
import test from 'node:test';
import { assertRunId, PaymentCredential } from '../../../src/agentvault/model/base.js';

test('base models enforce safe run identity and redact credentials', () => {
  assert.doesNotThrow(() => assertRunId('run-1'));
  assert.throws(() => assertRunId('unsafe/key'));
  const credential = new PaymentCredential(
    'x402', 'challenge-secret', 'credential-secret', { signature: 'signature-secret' },
    { bound_origin: 'https://merchant.test', header_name: 'PAYMENT-SIGNATURE', header_value: 'header-secret' },
  );
  const rendered = inspect(credential);
  for (const secret of ['challenge-secret', 'credential-secret', 'signature-secret', 'header-secret']) {
    assert.doesNotMatch(rendered, new RegExp(secret));
  }
});
