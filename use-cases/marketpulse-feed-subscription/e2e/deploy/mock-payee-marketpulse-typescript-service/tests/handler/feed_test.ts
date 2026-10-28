import assert from 'node:assert/strict';
import test from 'node:test';
import { FeedHandler } from '../../src/handler/feed.js';

test('FeedHandler exposes operational and MCP handlers', () => {
  assert.equal(typeof FeedHandler.prototype, 'object');
});

test('native Task delivery preserves application task scope and durable news evidence', async () => {
  const {InMemoryTaskStore} = await import('@a2a-js/sdk/server');
  const {default:express} = await import('express');
  const store = new InMemoryTaskStore();
  await store.save({kind:'task', id:'app-task', contextId:'app-context', status:{state:'submitted'}});
  let run = '';
  const repository = {delivery(runId:string) {run=runId; return {response:JSON.stringify({jsonrpc:'2.0', id:'run-1',
    result:{structuredContent:{news:[], paid:true, intentId:'inbound-1'}}})};}};
  const handler = new FeedHandler(repository as unknown as import('../../src/storage/feed.js').FeedSqliteRepository, false, store);
  const app = express();
  app.post('/a2a', express.raw({type:'application/json'}), handler.a2a);
  const server = app.listen(0, '127.0.0.1');
  await new Promise<void>(resolve => server.once('listening', resolve));
  try {
    const address = server.address() as import('node:net').AddressInfo;
    const response = await fetch(`http://127.0.0.1:${address.port}/a2a`, {method:'POST',
      headers:{'content-type':'application/json','x-run-id':'run-1'}, body:JSON.stringify({jsonrpc:'2.0', id:'rpc-1',
        method:'message/send', params:{message:{kind:'message', messageId:'message-1', taskId:'app-task', contextId:'app-context',
          role:'user', parts:[{kind:'text', text:'news'}]}}})});
    const value = await response.json() as {result:{id:string;contextId:string;artifacts:{parts:{data:{intentId:string}}[]}[]}};
    assert.equal(response.status, 200);
    assert.equal(value.result.id, 'app-task');
    assert.equal(value.result.contextId, 'app-context');
    assert.equal(value.result.artifacts[0]?.parts[0]?.data.intentId, 'inbound-1');
    assert.equal(run, 'run-1');
  } finally {await new Promise<void>((resolve,reject)=>server.close(error=>error?reject(error):resolve()));}
});
