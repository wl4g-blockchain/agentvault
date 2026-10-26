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


if __name__ == "__main__":
    unittest.main()
