import asyncio
import unittest

import httpx

from agentvault.config import PayerConfig
from agentvault.model import AssetIdentity, PayerRequestContext
from agentvault.payer.payer import PayerMiddleware
from agentvault.payer.standard import HttpxPayerMiddleware


class _Transport:
    def request(self, *_args, **_kwargs):
        raise AssertionError("oversized challenges must be rejected before AgentVault is called")


class StandardPayerMiddlewareTest(unittest.TestCase):
    def test_operation_key_is_stable(self):
        self.assertEqual(
            PayerRequestContext.for_operation("run-1", "realtime"),
            PayerRequestContext.for_operation("run-1", "realtime"),
        )

    def test_oversized_challenge_is_rejected_while_streaming(self):
        async def execute() -> None:
            async def respond(_request: httpx.Request) -> httpx.Response:
                return httpx.Response(402, content=b"x" * 9)

            client = httpx.AsyncClient(
                transport=httpx.MockTransport(respond), follow_redirects=False)
            core = PayerMiddleware(_Transport(), PayerConfig(
                "scenario", "wallet", "x402", AssetIdentity("eip155:1", "USDC", 6),
                "100", "recipient", max_challenge_bytes=8))
            middleware = HttpxPayerMiddleware(core, client=client)
            try:
                with self.assertRaisesRegex(RuntimeError, "challenge exceeds"):
                    await middleware.post(
                        "https://merchant.test/mcp",
                        operation=PayerRequestContext.for_operation("run-1", "paid"),
                        content=b"{}",
                    )
            finally:
                await client.aclose()

        asyncio.run(execute())
