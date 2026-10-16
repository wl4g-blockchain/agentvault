import asyncio
import json
import unittest

from agentvault.config import PayeeConfig
from agentvault.payee.fast import FastPayeeMiddleware, FastPayeeRoute
from agentvault.payee.payee import PayeeMiddleware


class _NoopTransport:
    def request(self, *_args, **_kwargs):
        raise AssertionError("Vault must not be called for the free tool")


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


async def _body(receive):
    return (await receive()).get("body", b"")
