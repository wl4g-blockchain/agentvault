import assert from 'node:assert/strict';
import test from 'node:test';
import {readFileSync} from 'node:fs';
import {A2ACall} from '../../../src/agentvault/model/a2a.js';
import {PaymentProtocolFactory} from '../../../src/agentvault/protocol/adapter.js';

test('A2A shared native contract, Cartesian matrix and unsupported carriers',()=>{
  const fixture=JSON.parse(readFileSync(new URL('../../../../tests/payment_protocol_a2a.json',import.meta.url),'utf8'));
  const registry=new PaymentProtocolFactory().create(['x402','mpp']);
  const expected=new Set(['grpc','mqtt'].flatMap(t=>['x402','mpp'].flatMap(p=>['payer','payee'].map(r=>`${t}/${p}/${r}`))));
  assert.equal(fixture.matrix.length,8);
  assert.deepEqual(new Set(fixture.matrix.map((row:any)=>`${row.transport}/${row.protocol}/${row.role}`)),expected);
  for(const [name,value] of Object.entries(fixture.protocols)){
    const contract=value as any;
    assert.equal(registry.a2aChallenge(contract.challenge_response,65536)?.[0].name,name);
    const request=structuredClone(contract.credential_request);
    const call=A2ACall.parse(Buffer.from(JSON.stringify(request)),65536)!;
    assert.equal(call.params().message.taskId,'task-1');
    assert.equal(registry.a2aCredential(request,65536)[0],name);
    assert.equal(registry.a2aEvidence(contract.settled_response,65536)[0],name);
    registry.a2aStripCredentials(request);
    assert.equal(registry.a2aCredential(request,65536)[1],undefined);
    assert.deepEqual(request.params.message.metadata,{'application.label':'keep'});
    assert.throws(()=>call.bindTask({...A2ACall.task(contract.settled_response),contextId:'wrong'}));
  }
  for(const method of ['message/stream','SendMessage','SendStreamingMessage'])assert.throws(()=>A2ACall.parse(Buffer.from(JSON.stringify({...fixture.request,method})),65536));
  const combined=structuredClone(fixture.protocols.x402.credential_request);combined._meta=fixture.protocols.mpp.credential_request._meta;
  assert.throws(()=>registry.a2aCredential(combined,65536));
  const embedded=structuredClone(fixture.protocols.x402.challenge_response);delete embedded.result.status.message.metadata['x402.payment.required'];
  assert.throws(()=>registry.a2aChallenge(embedded,65536));
});
