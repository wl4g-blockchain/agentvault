from __future__ import annotations

import json
import os
from contextlib import AsyncExitStack
from dataclasses import dataclass
from typing import Any

import httpx
from mcp import ClientSession
from mcp.client.streamable_http import streamable_http_client

from agentvault import (
    AgentVault,
    MqttConfig,
    MqttVaultTransport,
    PayerConfig,
    PayerRequestContext,
)

YESTERDAY_TOOL = "marketpulse.yesterday_news"
REALTIME_TOOL = "marketpulse.realtime_news"


@dataclass(frozen=True)
class ToolResult:
    body: bytes
    payment_intent_id: str | None = None


class MCPHelper:
    """Owns one official MCP client and its two discovered remote tools."""

    def __init__(self, stack: AsyncExitStack, session: ClientSession,
                 payer_transport: _PayerMCPTransport,
                 mqtt: MqttVaultTransport, tools: set[str]) -> None:
        self._stack = stack
        self._session = session
        self._payer_transport = payer_transport
        self._mqtt = mqtt
        self.tools = frozenset(tools)

    @classmethod
    async def from_environment(cls) -> "MCPHelper":
        timeout = float(_positive("AGENTVAULT_MQTT_TIMEOUT_SECONDS"))
        mqtt = MqttVaultTransport(MqttConfig(
            host=_required("AGENTVAULT_MQTT_HOST"),
            port=_positive("AGENTVAULT_MQTT_PORT"),
            tls=_boolean("AGENTVAULT_MQTT_TLS"),
            username=_required("AGENTVAULT_MQTT_USERNAME"),
            password=_required("AGENTVAULT_MQTT_PASSWORD"),
            connection_prefix="quantstream-python",
            vault_client_id=_required("QUANTSTREAM_CLIENT_ID"),
            timeout=timeout,
        ))
        mqtt.connect()
        core = AgentVault(mqtt).payer(PayerConfig(
            scenario_id=_required("QUANTSTREAM_SCENARIO_ID"),
            approval_timeout=float(_positive("AGENTVAULT_APPROVAL_TIMEOUT_SECONDS")),
            delivery_retries=1,
        ))
        payer = core.standard(timeout=timeout)
        payer_transport = _PayerMCPTransport(payer)
        stack = AsyncExitStack()
        try:
            http = await stack.enter_async_context(httpx.AsyncClient(
                transport=payer_transport,
                timeout=httpx.Timeout(timeout, read=300.0),
                follow_redirects=False,
            ))
            streams = await stack.enter_async_context(streamable_http_client(
                _required("MARKETPULSE_URL"), http_client=http,
            ))
            session = await stack.enter_async_context(ClientSession(streams[0], streams[1]))
            await session.initialize()
            listed = await session.list_tools()
            tools = {tool.name for tool in listed.tools}
            for required_tool in (YESTERDAY_TOOL, REALTIME_TOOL):
                if required_tool not in tools:
                    raise RuntimeError(f"MarketPulse MCP server did not expose {required_tool}")
            return cls(stack, session, payer_transport, mqtt, tools)
        except BaseException:
            await stack.aclose()
            await payer.close()
            mqtt.close()
            raise

    async def close(self) -> None:
        await self._stack.aclose()
        await self._payer_transport.close()
        self._mqtt.close()

    async def yesterday_news(self, run_id: str) -> ToolResult:
        return await self._call(YESTERDAY_TOOL, run_id, False)

    async def realtime_news(self, run_id: str, fail_after_settlement: bool = False) -> ToolResult:
        return await self._call(REALTIME_TOOL, run_id, fail_after_settlement)

    async def _call(self, tool: str, run_id: str, fail_after_settlement: bool) -> ToolResult:
        if tool not in self.tools:
            raise RuntimeError(f"MarketPulse MCP tool {tool!r} is not registered")
        metadata = {
            "run_id": run_id,
            "paid": tool == REALTIME_TOOL,
            "fail_after_settlement": fail_after_settlement,
        }
        result = await self._session.call_tool(tool, {}, meta={"agentvault": metadata})
        if result.isError:
            raise RuntimeError(f"MarketPulse MCP tool {tool} returned an application error")
        body = json.dumps(result.structuredContent or {}, separators=(",", ":")).encode()
        intent_id = self._payer_transport.take_payment(run_id)
        if tool == REALTIME_TOOL and not intent_id:
            raise RuntimeError("paid MCP result omitted AgentVault payment metadata")
        return ToolResult(body, intent_id)


class _PayerMCPTransport(httpx.AsyncBaseTransport):
    def __init__(self, payer: Any) -> None:
        self._payer = payer
        self._payments: dict[str, str] = {}

    async def aclose(self) -> None:
        # The helper owns the payer lifecycle separately from httpx's transport.
        return None

    async def close(self) -> None:
        await self._payer.close()

    def take_payment(self, run_id: str) -> str | None:
        return self._payments.pop(run_id, None)

    async def handle_async_request(self, request: httpx.Request) -> httpx.Response:
        body = await request.aread()
        metadata = _payment_metadata(body)
        headers = dict(request.headers)
        operation = None
        if metadata is not None:
            headers["x-run-id"] = metadata["run_id"]
            if metadata["fail_after_settlement"]:
                headers["x-e2e-fail-after-settlement"] = "once"
            operation = PayerRequestContext(metadata["run_id"])
        response = await self._payer.request(
            request.method, str(request.url), operation=operation,
            headers=headers, content=body,
        )
        content = await response.aread()
        if metadata is not None:
            payment = response.extensions.get("agentvault.payment")
            if payment is not None:
                self._payments[metadata["run_id"]] = payment.intent.id
        return httpx.Response(
            response.status_code, headers=response.headers, content=content, request=request,
        )


def _payment_metadata(body: bytes) -> dict[str, Any] | None:
    try:
        value = json.loads(body)
        if value.get("method") != "tools/call":
            return None
        metadata = value["params"]["_meta"]["agentvault"]
        if not isinstance(metadata, dict):
            return None
        if not isinstance(metadata.get("run_id"), str):
            return None
        if not isinstance(metadata.get("paid"), bool) or not isinstance(metadata.get("fail_after_settlement"), bool):
            return None
        return metadata
    except (KeyError, TypeError, ValueError, json.JSONDecodeError):
        return None


def _required(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise RuntimeError(f"{name} is required")
    return value


def _positive(name: str) -> int:
    value = int(_required(name))
    if value <= 0:
        raise RuntimeError(f"{name} must be positive")
    return value


def _boolean(name: str) -> bool:
    value = _required(name).lower()
    if value not in {"true", "false"}:
        raise RuntimeError(f"{name} must be true or false")
    return value == "true"
