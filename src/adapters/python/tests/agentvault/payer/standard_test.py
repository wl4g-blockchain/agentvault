import asyncio
import unittest

import httpx

from agentvault.config import PayerConfig
from agentvault.model import PayerRequestContext
from agentvault.payer.payer import PayerMiddleware
from agentvault.payer.standard import HttpxPayerMiddleware


class _Transport:
    def request(self, *_args, **_kwargs):
        raise AssertionError("oversized challenges must be rejected before AgentVault is called")


class StandardPayerMiddlewareTest(unittest.TestCase):
    def test_run_id_is_stable(self):
        self.assertEqual(
            PayerRequestContext("run-1"),
            PayerRequestContext("run-1"),
        )

    def test_oversized_challenge_is_rejected_while_streaming(self):
        async def execute() -> None:
            async def respond(_request: httpx.Request) -> httpx.Response:
                return httpx.Response(402, content=b"x" * 9)

            client = httpx.AsyncClient(
                transport=httpx.MockTransport(respond), follow_redirects=False)
            core = PayerMiddleware(_Transport(), PayerConfig("scenario", max_challenge_bytes=8))
            middleware = HttpxPayerMiddleware(core, client=client)
            try:
                with self.assertRaisesRegex(RuntimeError, "challenge exceeds"):
                    await middleware.post(
                        "https://merchant.test/mcp",
                        operation=PayerRequestContext("run-1"),
                        content=b"{}",
                    )
            finally:
                await client.aclose()

        asyncio.run(execute())

    def test_observability_message_keeps_run_identity(self):
        from agentvault.payer.standard import _event

        with self.assertLogs("agentvault.sdk", level="INFO") as captured:
            _event("agentvault.sdk.payer.signed", run_id="run-1", intent_id="intent-1")
        self.assertIn("run.id=run-1", captured.output[0])
        self.assertIn("intent.id=intent-1", captured.output[0])
