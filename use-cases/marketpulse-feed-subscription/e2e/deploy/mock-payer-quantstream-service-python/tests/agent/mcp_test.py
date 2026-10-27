from __future__ import annotations

import os
import unittest
from unittest.mock import patch

from app.agent.mcp import ToolResult, _boolean, _positive, _required


class MCPHelperTest(unittest.TestCase):
    def test_environment_values_are_strictly_typed(self) -> None:
        with patch.dict(os.environ, {"VALUE": "7", "FLAG": "true"}, clear=False):
            self.assertEqual(_required("VALUE"), "7")
            self.assertEqual(_positive("VALUE"), 7)
            self.assertTrue(_boolean("FLAG"))

    def test_tool_result_separates_body_and_payment_evidence(self) -> None:
        result = ToolResult(b'{"paid":true}', "intent-python-1")
        self.assertEqual(result.payment_intent_id, "intent-python-1")

class A2APayerTest(unittest.IsolatedAsyncioTestCase):
    async def test_native_send_uses_existing_payer_middleware_and_payment_extension(self):
        from types import SimpleNamespace
        import httpx
        from app.agent.mcp import MCPHelper
        class Payer:
            async def post(self, url, **kwargs):
                self.call = (url, kwargs)
                return httpx.Response(200, request=httpx.Request("POST", url), json={"result":{
                    "kind":"task", "id":"app-task", "contextId":"context-1", "status":{"state":"completed"},
                    "artifacts":[{"artifactId":"news", "parts":[{"kind":"data", "data":{"news":[], "paid":True}}]}]}},
                    extensions={"agentvault.payment":SimpleNamespace(intent=SimpleNamespace(id="outbound-1", protocol="x402"))})
        payer = Payer()
        helper = MCPHelper(None, None, SimpleNamespace(_payer=payer), None, set())
        with patch.dict(os.environ, {"MARKETPULSE_URL":"http://merchant/mcp"}):
            result = await helper._a2a("run-1", False)
        self.assertEqual(result.payment_intent_id, "outbound-1")
        self.assertEqual(payer.call[0], "http://merchant/a2a")
        self.assertEqual(payer.call[1]["json"]["method"], "message/send")
        self.assertNotIn("taskId", payer.call[1]["json"]["params"]["message"])


if __name__ == "__main__":
    unittest.main()
