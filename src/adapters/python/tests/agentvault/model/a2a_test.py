import copy
import json
import unittest
from pathlib import Path
from agentvault.model.a2a import A2ACall
from agentvault.protocol import PaymentProtocolFactory

class A2ACallTest(unittest.TestCase):
    def test_shared_native_contract_and_rejection_boundaries(self):
        fixture = json.loads((Path(__file__).resolve().parents[4] / 'tests/payment_protocol_a2a.json').read_text())
        registry = PaymentProtocolFactory().create(('x402', 'mpp'))
        expected = {(t, p, r) for t in ('grpc','mqtt') for p in ('x402','mpp') for r in ('payer','payee')}
        self.assertEqual(len(fixture['matrix']), len(expected))
        self.assertEqual({(row['transport'],row['protocol'],row['role']) for row in fixture['matrix']}, expected)
        for name, contract in fixture['protocols'].items():
            self.assertEqual(registry.a2a_challenge(contract['challenge_response'],65536)[0].name,name)
            request = copy.deepcopy(contract['credential_request'])
            call = A2ACall.parse(json.dumps(request).encode(),65536)
            self.assertEqual(call.params().message.task_id,'task-1')
            self.assertEqual(registry.a2a_credential(request,65536)[0],name)
            self.assertEqual(registry.a2a_evidence(contract['settled_response'],65536)[0],name)
            registry.a2a_strip_credentials(request)
            self.assertIsNone(registry.a2a_credential(request,65536)[1])
            self.assertEqual(request['params']['message']['metadata'],{'application.label':'keep'})
            task=A2ACall.task(contract['settled_response'])
            task.context_id='wrong-context'
            with self.assertRaises(ValueError):call.bind_task(task)
        for method in ('message/stream','SendMessage','SendStreamingMessage'):
            request = {**fixture['request'], 'method': method}
            with self.assertRaises(ValueError):A2ACall.parse(json.dumps(request).encode(),65536)
        combined=copy.deepcopy(fixture['protocols']['x402']['credential_request'])
        combined['_meta']=fixture['protocols']['mpp']['credential_request']['_meta']
        with self.assertRaises(ValueError):registry.a2a_credential(combined,65536)
        embedded=copy.deepcopy(fixture['protocols']['x402']['challenge_response'])
        del embedded['result']['status']['message']['metadata']['x402.payment.required']
        with self.assertRaises(ValueError):registry.a2a_challenge(embedded,65536)
