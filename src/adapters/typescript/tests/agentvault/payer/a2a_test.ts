import assert from 'node:assert/strict';
import test from 'node:test';
import {readFileSync} from 'node:fs';
import {PayerMiddleware} from '../../../src/agentvault/payer/payer.js';
import {PayerRequestContext} from '../../../src/agentvault/model/payer.js';

test('A2A extension acknowledgement is mandatory before Vault authorization',async()=>{
  const fixture=JSON.parse(readFileSync(new URL('../../../../tests/payment_protocol_a2a.json',import.meta.url),'utf8'));
  const core=new PayerMiddleware({async request(){throw new Error('Vault must not be called');}}, {scenarioId:'scenario'});
  await assert.rejects(core.a2a().authorize({method:'POST',url:'https://merchant.test/a2a',origin:'https://merchant.test',headers:new Headers(),body:Buffer.from(JSON.stringify(fixture.request)),followRedirects:false},
    fixture.protocols.x402.challenge_response,new PayerRequestContext('run-1'),new Headers()),/acknowledged/);
  assert.equal(core.a2a().extensions(fixture.x402_extension),fixture.x402_extension);
});
