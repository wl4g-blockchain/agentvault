import asyncio
import copy
import json
import unittest
from pathlib import Path

import httpx
from a2a.server.tasks import InMemoryTaskStore
from a2a.types import Task, TaskStatus, MessageSendParams
from agentvault.config import PayeeConfig, PayerConfig
from agentvault.model import PayeeOperation, PayerRequestContext
from agentvault.payee.fast import FastPayeeRoute
from agentvault.payee.payee import PayeeMiddleware
from agentvault.payer.payer import PayerMiddleware
from tests.agentvault.payee.fast_test import _McpPayeeTransport
from tests.agentvault.payer.standard_test import _McpTransport


class _A2ATransport(_McpTransport):
    def _intent(self, status):
        value = super()._intent(status)
        value['resource'] = 'https://merchant.test/a2a'
        return value


class A2APayeeMiddlewareTest(unittest.IsolatedAsyncioTestCase):
    async def test_both_roles_native_tasks_bearer_and_receipt_after_business_error(self):
        root = Path(__file__).resolve().parents[4] / 'tests'
        carrier = json.loads((root / 'payment_protocol_a2a.json').read_text())
        protocols = json.loads((root / 'payment_protocol_mcp.json').read_text())['protocols']
        for protocol in protocols:
            for failure in (False, True):
                with self.subTest(protocol=protocol, failure=failure):
                    store = InMemoryTaskStore()
                    await store.save(Task(id='task-1', context_id='context-1', status=TaskStatus(state='submitted')))
                    transport = _McpPayeeTransport(protocol, protocols[protocol])
                    payee = PayeeMiddleware(transport, PayeeConfig(protocols=(protocol,)))
                    payer_transport = _A2ATransport(protocol, protocols[protocol])
                    payer = PayerMiddleware(payer_transport, PayerConfig('scenario', protocols=(protocol,)))
                    calls = []

                    async def app(scope, receive, send):
                        message = json.loads((await receive())['body'])
                        params = MessageSendParams.model_validate(message['params'])
                        self.assertEqual(params.message.task_id, 'task-1')
                        self.assertEqual(params.message.metadata, {'application.label': 'keep'})
                        self.assertEqual(dict(scope['headers'])[b'authorization'], b'Bearer application-token')
                        calls.append(message)
                        task = await store.get(params.message.task_id)
                        if failure:
                            response = {'jsonrpc': '2.0', 'id': message['id'], 'error': {'code': -32603, 'message': 'business failed'}}
                        else:
                            task.status = TaskStatus(state='completed')
                            response = {'jsonrpc': '2.0', 'id': message['id'], 'result': task.model_dump(by_alias=True, exclude_none=True)}
                        await send({'type': 'http.response.start', 'status': 200, 'headers': [(b'content-type', b'application/json')]})
                        await send({'type': 'http.response.body', 'body': json.dumps(response).encode()})

                    protected = payee.fast(app, [FastPayeeRoute('POST', '/a2a', 'scenario', 'price',
                        'mcp://marketpulse/realtime', 'https://marketpulse.example', protocol, a2a_task_store=store)])
                    async with httpx.AsyncClient(transport=httpx.ASGITransport(protected)) as http:
                        client = payer.standard(client=http)
                        result = await client.post('https://merchant.test/a2a', operation=PayerRequestContext('run-1'),
                            headers={'authorization': 'Bearer application-token'}, json=copy.deepcopy(carrier['request']))
                    self.assertEqual(result.status_code, 200, result.text)
                    self.assertEqual(result.json()['result']['status']['state'], 'failed' if failure else 'completed')
                    self.assertEqual(len(calls), 1)
                    self.assertEqual(len(set(transport.request_hashes)), 1)
                    self.assertEqual(len(payer_transport.calls), 2)
                    stored = await store.get('task-1')
                    self.assertEqual(stored.metadata['agentvault.payment.run_id'], 'run-1')
                    self.assertEqual(stored.context_id, 'context-1')
                    report = payer_transport.calls[-1][2]
                    self.assertEqual(report.get('protocol', 'x402'), protocol)

    async def test_existing_task_rejects_another_payment_run(self):
        root = Path(__file__).resolve().parents[4] / 'tests'
        carrier = json.loads((root / 'payment_protocol_a2a.json').read_text())
        protocol = json.loads((root / 'payment_protocol_mcp.json').read_text())['protocols']['x402']
        store = InMemoryTaskStore()
        await store.save(Task(id='task-1', context_id='context-1', status=TaskStatus(state='submitted'),
            metadata={'agentvault.payment.run_id': 'other-run'}))
        core = PayeeMiddleware(_McpPayeeTransport('x402', protocol), PayeeConfig())
        operation = PayeeOperation('scenario', 'run-1', 'price', 'feed', 'https://merchant.test', 'x402', 'request-hash')
        with self.assertRaisesRegex(ValueError, 'payment scope'):
            await core.a2a().prepare(json.dumps(carrier['request']).encode(), operation, store,
                {'x-a2a-extensions': carrier['x402_extension']})
