from __future__ import annotations

import asyncio
import urllib.parse
from typing import Any, Mapping

import httpx

from ..model import PayerHttpRequest, PayerHttpResponse, PayerRequestContext
from .payer import PayerMiddleware

class HttpxPayerMiddleware:
    """Async HTTPX payer entrypoint with bounded, redirect-free retries."""

    def __init__(self, core: PayerMiddleware, *, timeout: float = 30.0,
                 client: httpx.AsyncClient | None = None) -> None:
        if timeout <= 0:
            raise ValueError("payer HTTP timeout must be positive")
        self.core = core
        self._owned = client is None
        self._client = client or httpx.AsyncClient(timeout=timeout, follow_redirects=False)
        if self._client.follow_redirects:
            raise ValueError("payer HTTP client must disable redirects")

    async def __aenter__(self) -> "HttpxPayerMiddleware":
        return self

    async def __aexit__(self, *_: object) -> None:
        await self.close()

    async def close(self) -> None:
        if self._owned:
            await self._client.aclose()

    async def request(self, method: str, url: str, *, operation: PayerRequestContext | None = None,
                      headers: Mapping[str, str] | None = None,
                      content: bytes | str | None = None, **kwargs: Any) -> httpx.Response:
        if kwargs.get("follow_redirects") is True:
            raise ValueError("paid requests cannot follow redirects")
        if operation is not None and (
            (content is not None and not isinstance(content, (bytes, str)))
            or "files" in kwargs
        ):
            raise ValueError("paid requests must use a replayable, non-streaming body")
        request_headers = dict(headers or {})
        if operation is None:
            return await self._send(method, url, request_headers, content, **kwargs)
        first = await self._send(method, url, request_headers, content, stream=True, **kwargs)
        if first.status_code != 402:
            await first.aread()
            return first
        origin = _canonical_origin(url)
        try:
            challenge_body = await _read_bounded(first, self.core.config.max_challenge_bytes)
        finally:
            await first.aclose()
        authorized = await asyncio.to_thread(
            self.core.authorize,
            PayerHttpRequest(method, url, origin, request_headers, _body_bytes(content), False),
            PayerHttpResponse(first.status_code, dict(first.headers), challenge_body),
            operation,
        )
        if authorized is None:
            raise RuntimeError("AgentVault did not authorize the 402 challenge")
        name, value = self.core.credential_header(authorized, origin, request_headers)
        paid_headers = {**request_headers, name: value}
        response = first
        for attempt in range(self.core.config.delivery_retries + 1):
            response = await self._send(method, url, paid_headers, content, **kwargs)
            await asyncio.to_thread(
                self.core.report, operation, authorized,
                PayerHttpResponse(response.status_code, dict(response.headers)))
            if response.status_code != 503 or attempt == self.core.config.delivery_retries:
                response.extensions["agentvault.payment"] = authorized
                return response
        raise RuntimeError("payer retry loop ended unexpectedly")

    async def get(self, url: str, **kwargs: Any) -> httpx.Response:
        return await self.request("GET", url, **kwargs)

    async def post(self, url: str, **kwargs: Any) -> httpx.Response:
        return await self.request("POST", url, **kwargs)

    async def _send(self, method: str, url: str, headers: Mapping[str, str],
                    content: bytes | str | None, *, stream: bool = False,
                    **kwargs: Any) -> httpx.Response:
        request = self._client.build_request(method, url, headers=headers, content=content, **kwargs)
        return await self._client.send(request, follow_redirects=False, stream=stream)


async def _read_bounded(response: httpx.Response, maximum: int) -> bytes:
    declared = response.headers.get("content-length")
    if declared is not None:
        try:
            if int(declared) > maximum:
                raise RuntimeError("payment challenge exceeds configured limit")
        except ValueError as error:
            raise RuntimeError("payment challenge content-length is invalid") from error
    chunks: list[bytes] = []
    length = 0
    async for chunk in response.aiter_bytes():
        length += len(chunk)
        if length > maximum:
            raise RuntimeError("payment challenge exceeds configured limit")
        chunks.append(chunk)
    return b"".join(chunks)


def _canonical_origin(url: str) -> str:
    parsed = urllib.parse.urlsplit(url)
    if parsed.scheme not in {"http", "https"} or not parsed.netloc or parsed.username:
        raise ValueError("paid request URL must be absolute HTTP(S) without userinfo")
    return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, "", "", ""))


def _body_bytes(value: bytes | str | None) -> bytes:
    if value is None:
        return b""
    return value if isinstance(value, bytes) else value.encode()
