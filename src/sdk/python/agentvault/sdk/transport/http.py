from __future__ import annotations
import json
import logging
import secrets
import httpx
from typing import Any, Mapping
from ..config import HttpConfig
from .transport import APIError

_LOG = logging.getLogger("agentvault.sdk.transport.http")


class HttpVaultTransport:
    def __init__(self, config: HttpConfig) -> None:
        self.config = config
        self._client = httpx.Client(timeout=config.timeout, follow_redirects=False)

    def __repr__(self) -> str:
        return f"HttpVaultTransport(config={self.config!r})"

    def request(
        self, method: str, path: str, body: Mapping[str, Any] | None = None
    ) -> dict[str, Any]:
        if (
            method not in ("GET", "POST")
            or not path.startswith("/api/v1/")
            or "\\" in path
        ):
            raise ValueError("invalid AgentVault HTTP request")
        payload = (
            b""
            if body is None
            else json.dumps(body, separators=(",", ":"), ensure_ascii=False).encode()
        )
        if len(payload) > self.config.max_response_bytes:
            raise ValueError("AgentVault HTTP request exceeds configured limit")
        trace_id, span_id = secrets.token_hex(16), secrets.token_hex(8)
        headers = {
            "accept": "application/json",
            "accept-encoding": "identity",
            "traceparent": f"00-{trace_id}-{span_id}-01",
        }
        if body is not None:
            headers["content-type"] = "application/json"
        if self.config.authguard_bearer:
            headers["authorization"] = "Bearer " + self.config.authguard_bearer
        _LOG.info(
            "agentvault.sdk.transport.http.produced trace_id=%s span_id=%s parent_id=",
            trace_id,
            span_id,
            extra={
                "event_name": "agentvault.sdk.transport.http.produced",
                "trace_id": trace_id,
                "span_id": span_id,
                "parent_id": "",
            },
        )
        with self._client.stream(
            method,
            self.config.base_url.rstrip("/") + path,
            content=payload,
            headers=headers,
        ) as response:
            data = bytearray()
            for chunk in response.iter_raw():
                if len(data) + len(chunk) > self.config.max_response_bytes:
                    raise RuntimeError(
                        "AgentVault HTTP response exceeds configured limit"
                    )
                data.extend(chunk)
            if not 200 <= response.status_code < 300:
                try:
                    detail = json.loads(data).get("error", {})
                except (ValueError, AttributeError):
                    detail = {}
                if not isinstance(detail, dict):
                    detail = {}
                raise APIError(
                    response.status_code,
                    str(detail.get("code", "TRANSPORT_ERROR")),
                    str(detail.get("message", "AgentVault HTTP request failed")),
                )
            value = json.loads(data)
            if not isinstance(value, dict):
                raise RuntimeError("invalid AgentVault HTTP response")
            return value

    def close(self) -> None:
        self._client.close()

    def __enter__(self) -> HttpVaultTransport:
        return self

    def __exit__(self, *_: object) -> None:
        self.close()
