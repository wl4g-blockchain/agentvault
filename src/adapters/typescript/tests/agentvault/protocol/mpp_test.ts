import assert from 'node:assert/strict';
import test from 'node:test';
import { Challenge, Credential } from 'mppx';
import { PaymentCredential, type PaymentChallenge } from '../../../src/agentvault/model/base.js';
import { MppAdapter } from '../../../src/agentvault/protocol/mpp.js';

test('MppAdapter preserves Bearer and uses only native MPP headers', () => {
  const adapter = new MppAdapter();
  const native = Challenge.from({
    id: 'challenge-0123456789abcdef',
    realm: 'merchant.example',
    method: 'tempo',
    intent: 'charge',
    request: { amount: '1000', currency: 'USDC' },
    header: 'Payment-Authorization',
  });
  const challenge: PaymentChallenge = {
    scenario_id: 'scenario', offer_id: 'offer', resource: 'https://merchant.example/tool', protocol: 'mpp',
    asset: { network: 'eip155:42431', asset: 'USDC', decimals: 6 }, amount: '1000',
    recipient: '0x1111111111111111111111111111111111111111', nonce: 'nonce',
    challenge_hash: 'challenge', expires_at: 2_000_000_000,
    payload: { www_authenticate: Challenge.serialize(native) },
  };
  const response = adapter.challenge(challenge);
  assert.ok(response.headers['www-authenticate']);
  assert.equal(response.headers['payment-required'], undefined);
  const authorization = Credential.serialize(Credential.from({
    challenge: native,
    payload: { type: 'hash', hash: '0x1111' },
  }));
  assert.deepEqual(adapter.presentedCredential(new Headers({
    authorization: 'Bearer app-token',
    'payment-authorization': authorization,
  }), 1024), { authorization });
  assert.throws(() => adapter.presentedCredential(new Headers({
    authorization,
    'payment-authorization': authorization,
  }), 1024), /multiple/);
  assert.equal(adapter.credential(new PaymentCredential(
    'mpp', 'challenge', 'credential', {}, {
      bound_origin: 'https://merchant.example',
      header_name: 'payment-authorization',
      header_value: authorization,
    },
  )).header_name, 'payment-authorization');
  assert.throws(() => adapter.presentedCredential(new Headers({
    authorization,
  }), 1024), /challenged header/);
  assert.throws(() => adapter.credential(new PaymentCredential(
    'mpp', 'challenge', 'credential', {}, {
      bound_origin: 'https://merchant.example',
      header_name: 'Authorization',
      header_value: authorization,
    },
  )), /challenged header/);
});

test('MppAdapter rejects unimplemented methods and intents', () => {
  const adapter = new MppAdapter();
  for (const [method, intent] of [['stripe', 'charge'], ['tempo', 'session']] as const) {
    const challenge = Challenge.from({
      id: 'challenge-0123456789abcdef',
      realm: 'merchant.example',
      method,
      intent,
      request: { amount: '1000', currency: 'USDC' },
    });
    assert.throws(() => adapter.challenge({
      scenario_id: 'scenario',
      offer_id: 'offer',
      resource: 'https://merchant.example/tool',
      protocol: 'mpp',
      asset: { network: 'eip155:42431', asset: 'USDC', decimals: 6 },
      amount: '1000',
      recipient: 'recipient',
      nonce: 'nonce',
      challenge_hash: 'challenge',
      expires_at: 2_000_000_000,
      payload: { www_authenticate: Challenge.serialize(challenge) },
    }), /only the implemented Tempo charge/);
  }
});
