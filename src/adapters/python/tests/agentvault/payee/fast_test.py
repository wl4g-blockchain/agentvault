import asyncio
import json
import unittest
from types import SimpleNamespace
from unittest.mock import patch

from agentvault.config import PayeeConfig
from agentvault.payee.fast import FastPayeeMiddleware, FastPayeeRoute
from agentvault.payee.payee import PayeeMiddleware


class _NoopTransport:
    def request(self, *_args, **_kwargs):
        raise AssertionError("Vault must not be called for the free tool")


class _AuthorizedMiddleware:
    def protect(self, _operation):
        payment = SimpleNamespace(intent=SimpleNamespace(id="intent-1"))
        return SimpleNamespace(outcome="authorized", challenge=None, payment=payment)


class FastPayeeMiddlewareTest(unittest.TestCase):
    def test_route_is_exact(self):
        route = FastPayeeRoute.mcp_tool(
            "/mcp", "marketpulse.realtime_news", "scenario", "offer",
            "mcp://marketpulse/realtime", "https://marketpulse.example")
        self.assertEqual((route.method, route.path), ("POST", "/mcp"))
        self.assertEqual(route.mcp_tool, "marketpulse.realtime_news")

    def test_bypasses_free_tool_on_shared_mcp_route(self):
        called: list[bytes] = []

        async def app(_scope, receive, _send):
            called.append(await _body(receive))

        route = FastPayeeRoute.mcp_tool(
            "/mcp", "marketpulse.realtime_news", "scenario", "offer",
            "mcp://marketpulse/realtime", "https://marketpulse.example")
        middleware = FastPayeeMiddleware(
            app, middleware=PayeeMiddleware(_NoopTransport(), PayeeConfig()), routes=[route])
        body = json.dumps({"method": "tools/call", "params": {"name": "marketpulse.yesterday_news"}}).encode()
        delivered = False

        async def receive():
            nonlocal delivered
            if delivered:
                return {"type": "http.request", "body": b"", "more_body": False}
            delivered = True
            return {"type": "http.request", "body": body, "more_body": False}

        async def send(_message):
            return None

        asyncio.run(middleware(
            {"type": "http", "method": "POST", "path": "/mcp", "headers": []}, receive, send))
        self.assertEqual(called, [body])

    def test_resource_release_log_keeps_run_and_intent_identity(self):
        async def app(_scope, _receive, send):
            await send({"type": "http.response.start", "status": 200, "headers": []})
            await send({"type": "http.response.body", "body": b"{}"})

        route = FastPayeeRoute.mcp_tool(
            "/mcp", "marketpulse.realtime_news", "scenario", "offer",
            "mcp://marketpulse/realtime", "https://marketpulse.example")
        middleware = FastPayeeMiddleware(app, middleware=_AuthorizedMiddleware(), routes=[route])
        body = json.dumps({"method": "tools/call", "params": {"name": route.mcp_tool}}).encode()
        delivered = False

        async def receive():
            nonlocal delivered
            if delivered:
                return {"type": "http.request", "body": b"", "more_body": False}
            delivered = True
            return {"type": "http.request", "body": body, "more_body": False}

        async def send(_message):
            return None

        scope = {
            "type": "http", "method": "POST", "path": "/mcp",
            "headers": [(b"x-run-id", b"run-1"), (b"payment-signature", b"e30=")],
        }
        with patch("agentvault.payee.fast._payment_response", return_value="receipt"):
            with self.assertLogs("agentvault.sdk", level="INFO") as captured:
                asyncio.run(middleware(scope, receive, send))
        self.assertIn("run.id=run-1", captured.output[-1])
        self.assertIn("intent.id=intent-1", captured.output[-1])


async def _body(receive):
    return (await receive()).get("body", b"")
