from __future__ import annotations

import os
from contextlib import asynccontextmanager

import uvicorn
from agentvault import (
    AgentVault, FastPayeeMiddleware, FastPayeeRoute, GrpcConfig,
    GrpcVaultTransport, MqttConfig, MqttVaultTransport, VaultTransport,
)
from fastapi import FastAPI, Response

from app.handler.feed import FeedHandler
from app.route.feed import FeedRouter
from app.storage.feed import FeedSQLiteStorage


class MarketPulseServer:
    """Owns the FastAPI listener, MCP endpoint, SDK middleware and lifecycle."""

    def __init__(self, application: FastAPI, storage: FeedSQLiteStorage, host: str, port: int) -> None:
        self.application = application
        self._storage = storage
        self._host = host
        self._port = port

    @classmethod
    def from_environment(cls) -> "MarketPulseServer":
        host, port = _parse_bind(_required("MARKETPULSE_BIND"))
        transport = _payee_transport()
        payee = AgentVault(transport).payee()
        storage = FeedSQLiteStorage(_required("MARKETPULSE_DATABASE"))
        handler = FeedHandler(storage, os.environ.get("MARKETPULSE_E2E_ALLOW_FAILURE_INJECTION") == "true")

        @asynccontextmanager
        async def lifespan(_application: FastAPI):
            try:
                yield
            finally:
                close = getattr(transport, "close", None)
                if callable(close):
                    close()
                storage.close()

        application = FastAPI(title="MarketPulse Python Payee", version="0.1.0", lifespan=lifespan)
        FeedRouter(handler).register(application)
        application.add_api_route("/mcp", handler.mcp, methods=["POST"], response_class=Response)
        application.add_middleware(
            FastPayeeMiddleware,
            middleware=payee,
            routes=[FastPayeeRoute.mcp_tool(
                "/mcp",
                "marketpulse.realtime_news",
                _required("MARKETPULSE_SCENARIO_ID"),
                _required("MARKETPULSE_OFFER_ID"),
                _required("MARKETPULSE_RESOURCE_URL"),
                _required("MARKETPULSE_ORIGIN"),
            )],
        )

        return cls(application, storage, host, port)

    def run(self) -> None:
        uvicorn.run(self.application, host=self._host, port=self._port, access_log=False)

    def close(self) -> None:
        self._storage.close()


def _required(name: str) -> str:
    value = os.environ.get(name, "").strip()
    if not value:
        raise RuntimeError(f"{name} is required")
    return value


def _parse_bind(bind: str) -> tuple[str, int]:
    host, separator, port = bind.rpartition(":")
    if not separator or not host:
        raise RuntimeError("MARKETPULSE_BIND must be host:port")
    parsed = int(port)
    if not 0 < parsed <= 65535:
        raise RuntimeError("MARKETPULSE_BIND port is out of range")
    return host, parsed


def _payee_transport() -> VaultTransport:
    transport = os.environ.get("AGENTVAULT_TRANSPORT", "mqtt").strip().lower()
    if transport == "mqtt":
        return MqttVaultTransport(MqttConfig(
            host=_required("AGENTVAULT_MQTT_HOST"),
            port=int(_required("AGENTVAULT_MQTT_PORT")),
            tls=_boolean("AGENTVAULT_MQTT_TLS"),
            username=_required("AGENTVAULT_MQTT_USERNAME"),
            password=_required("AGENTVAULT_MQTT_PASSWORD"),
            connection_prefix="marketpulse-python",
            vault_client_id=_required("AGENTVAULT_MQTT_CLIENT_ID"),
            timeout=float(_required("AGENTVAULT_MQTT_TIMEOUT_SECONDS")),
        ))
    if transport == "grpc":
        return GrpcVaultTransport(GrpcConfig(
            endpoint=_required("AGENTVAULT_GRPC_ENDPOINT"),
            tls=_boolean("AGENTVAULT_GRPC_TLS"),
            timeout=float(_required("AGENTVAULT_GRPC_TIMEOUT_SECONDS")),
        ))
    raise RuntimeError("AGENTVAULT_TRANSPORT must be mqtt or grpc")


def _boolean(name: str) -> bool:
    value = os.environ.get(name, "false").strip().lower()
    if value not in {"true", "false"}:
        raise RuntimeError(f"{name} must be true or false")
    return value == "true"
