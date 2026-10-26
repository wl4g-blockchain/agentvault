from __future__ import annotations

import hashlib
import json
from typing import Any

from agentvault import FastPayeeMiddleware
from fastapi import Request, Response

from app.storage.feed import FeedSqliteRepository


class FeedHandler:
    def __init__(self, storage: FeedSqliteRepository, allow_failure_injection: bool) -> None:
        self._storage = storage
        self._allow_failure_injection = allow_failure_injection

    async def health(self) -> dict[str, str]:
        return {"status": "ready", "service": "marketpulse-python"}

    async def metrics(self) -> Response:
        return Response(
            content="# HELP marketpulse_up MarketPulse process readiness.\n"
                    "# TYPE marketpulse_up gauge\nmarketpulse_up 1\n",
            media_type="text/plain; version=0.0.4",
        )

    async def mcp(self, request: Request) -> Response:
        body = await request.body()
        try:
            rpc = json.loads(body)
            if rpc.get("jsonrpc") != "2.0":
                raise ValueError("invalid jsonrpc")
        except (ValueError, AttributeError):
            return self._response(400, self._error(None, -32600, "invalid request"))
        identifier = rpc.get("id")
        method = rpc.get("method")
        params = rpc.get("params") if isinstance(rpc.get("params"), dict) else {}
        if method == "initialize":
            version = params.get("protocolVersion", "2025-11-25")
            return self._response(200, self._result(identifier, {
                "protocolVersion": version,
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": "marketpulse-feed-python", "version": "0.1.0"},
            }))
        if method == "notifications/initialized":
            return Response(status_code=202)
        if method == "tools/list":
            return self._response(200, self._result(identifier, {"tools": self._tools()}))
        if method == "tools/call":
            return self._call_tool(request, body, identifier, params)
        return self._response(404, self._error(identifier, -32601, "method not found"))

    def _call_tool(self, request: Request, body: bytes, identifier: Any, params: dict[str, Any]) -> Response:
        name = params.get("name")
        if name == "marketpulse.yesterday_news":
            return self._response(200, self._tool_result(identifier, self._storage.news("yesterday", 20), False, None))
        if name != "marketpulse.realtime_news":
            return self._response(404, self._error(identifier, -32602, "unknown tool"))
        run_id = request.headers.get("x-run-id", "").strip()
        if not run_id:
            return self._response(400, self._error(identifier, -32602, "x-run-id is required"))
        request_hash = hashlib.sha256(b"marketpulse.mcp.request.v1\0" + body).hexdigest()
        try:
            delivery = self._storage.delivery(run_id, request_hash)
        except ValueError as error:
            return self._response(409, self._error(identifier, -32009, str(error)))
        if delivery.response:
            return Response(content=delivery.response, media_type="application/json")
        intent_id = delivery.intent_id
        if not intent_id:
            payment = FastPayeeMiddleware.settled_payment(request.scope)
            if payment is None:
                return self._response(402, self._error(identifier, -32005, "settled payment is required"))
            self._storage.record_settled(run_id, request_hash, payment)
            intent_id = payment.intent.id
        if (self._allow_failure_injection
                and request.headers.get("x-e2e-fail-after-settlement") == "once"
                and self._storage.record_failure(run_id, request_hash)):
            return self._response(503, self._error(identifier, -32004, "injected delivery failure after settlement"))
        payload = self._tool_result(identifier, self._storage.news("realtime", 20), True, intent_id)
        encoded = json.dumps(payload, separators=(",", ":"))
        self._storage.record_delivered(run_id, request_hash, encoded)
        return Response(content=encoded, media_type="application/json")

    @staticmethod
    def _tools() -> list[dict[str, Any]]:
        schema = {"type": "object", "additionalProperties": False}
        return [
            {"name": "marketpulse.yesterday_news", "description": "Free previous-day geopolitical and tariff intelligence", "inputSchema": schema},
            {"name": "marketpulse.realtime_news", "description": "Paid real-time geopolitical and tariff intelligence via x402", "inputSchema": schema},
        ]

    @classmethod
    def _tool_result(cls, identifier: Any, news: list[dict[str, Any]], paid: bool, intent_id: str | None) -> dict[str, Any]:
        structured = {"news": news, "paid": paid, "intentId": intent_id}
        return cls._result(identifier, {
            "content": [{"type": "text", "text": json.dumps(structured, separators=(",", ":"))}],
            "structuredContent": structured,
            "isError": False,
        })

    @staticmethod
    def _result(identifier: Any, result: Any) -> dict[str, Any]:
        return {"jsonrpc": "2.0", "id": identifier, "result": result}

    @staticmethod
    def _error(identifier: Any, code: int, message: str) -> dict[str, Any]:
        return {"jsonrpc": "2.0", "id": identifier, "error": {"code": code, "message": message}}

    @staticmethod
    def _response(status: int, value: dict[str, Any]) -> Response:
        return Response(content=json.dumps(value, separators=(",", ":")), status_code=status, media_type="application/json")
