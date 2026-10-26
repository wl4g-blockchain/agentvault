import json
import unittest
from unittest.mock import Mock
from pathlib import Path
from agentvault.config import PayerConfig
from agentvault.model import PayerHttpRequest, PayerRequestContext
from agentvault.payer.payer import PayerMiddleware

class A2APayerMiddlewareTest(unittest.TestCase):
    def test_extension_is_required_before_authorization(self):
        fixture=json.loads((Path(__file__).resolve().parents[4] / 'tests/payment_protocol_a2a.json').read_text())
        transport=Mock()
        core=PayerMiddleware(transport,PayerConfig('scenario'))
        request=PayerHttpRequest('POST','https://merchant.test/a2a','https://merchant.test',{'content-type':'application/json'},json.dumps(fixture['request']).encode())
        with self.assertRaisesRegex(ValueError,'acknowledged'):
            core.a2a().authorize(request,fixture['protocols']['x402']['challenge_response'],PayerRequestContext('run-1'),{})
        transport.request.assert_not_called()
        value=core.a2a().core._protocols.a2a_extensions(fixture['x402_extension'])
        self.assertEqual(value,fixture['x402_extension'])
