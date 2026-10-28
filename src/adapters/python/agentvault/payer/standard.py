from __future__ import annotations

import asyncio
import json
import logging
import urllib.parse
from typing import Any, Mapping

import httpx

from ..model import PayerHttpRequest, PayerHttpResponse, PayerRequestContext
from .payer import PayerMiddleware
from ..model.a2a import A2ACall

_LOG = logging.getLogger("agentvault.sdk")

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
            or "files" in kwargs or "data" in kwargs or "stream" in kwargs
        ):
            raise ValueError("paid requests must use a replayable, non-streaming body")
        request_headers = dict(headers or {})
        if operation is None:
            return await self._send(method, url, request_headers, content, **kwargs)
        if "json" in kwargs:
            if content is not None:
                raise ValueError("paid requests cannot provide both content and json")
            content = json.dumps(kwargs.pop("json"), separators=(",", ":"))
            if not any(name.lower() == "content-type" for name in request_headers):
                request_headers["content-type"] = "application/json"
        request_body = HttpxPayerMiddleware._body_bytes(content)
        mcp_request = self._mcp_request(method, request_headers, request_body)
        a2a_call = A2ACall.parse(request_body, self.core.config.max_challenge_bytes) if method.upper() == "POST" else None
        a2a = self.core.a2a() if a2a_call is not None else None
        if a2a_call is not None:
            for name, value in list(request_headers.items()):
                if name.lower() == "x-run-id" and value != operation.run_id:
                    raise ValueError("A2A run_id conflicts with paid operation")
                if name.lower() == "x-run-id":
                    request_headers.pop(name)
            request_headers["x-run-id"] = operation.run_id
            existing = next((value for name, value in request_headers.items() if name.lower() == "x-a2a-extensions"), "")
            request_headers = {name: value for name, value in request_headers.items() if name.lower() != "x-a2a-extensions"}
            request_headers["x-a2a-extensions"] = self.core._protocols.a2a_extensions(existing)
        first = await self._send(method, url, request_headers, content, stream=True, **kwargs)
        if first.status_code != 402:
            if (mcp_request is None and a2a_call is None) or not first.headers.get(
                "content-type", ""
            ).lower().startswith("application/json"):
                await first.aread()
                return first
            try:
                first_body = await HttpxPayerMiddleware._read_bounded(
                    first, self.core.config.max_challenge_bytes
                )
            finally:
                await first.aclose()
            try:
                message = json.loads(first_body)
            except (UnicodeDecodeError, ValueError):
                return self._buffered(first, first_body)
            request_id = a2a_call.envelope["id"] if a2a_call else self.core._mcp_call_id(request_body)
            if request_id is None or not self.core._bound_mcp_response(
                message, request_id, require_result=False
            ):
                return self._buffered(first, first_body)
            origin = HttpxPayerMiddleware._canonical_origin(url)
            authorized = await asyncio.to_thread(
                a2a.authorize if a2a else self.core.authorize_mcp,
                PayerHttpRequest(
                    method, url, origin, request_headers, request_body, False
                ),
                message,
                operation,
                first.headers,
            )
            if authorized is None:
                return self._buffered(first, first_body)
            HttpxPayerMiddleware._event("agentvault.sdk.payer.payment_required", run_id=operation.run_id)
            HttpxPayerMiddleware._event(
                "agentvault.sdk.payer.signed",
                run_id=operation.run_id,
                intent_id=authorized.intent.id,
            )
            if a2a is not None:
                paid_body = a2a.credential(authorized, a2a_call, message, origin)
            else:
                metadata = self.core.mcp_credential(authorized, origin)
                paid_body = self._attach_mcp_credential(mcp_request, metadata.key, metadata.value)
            paid_response_body: bytes | None = None

            async def report_mcp(response: httpx.Response) -> None:
                nonlocal paid_response_body
                if response.status_code == 503 or not 200 <= response.status_code < 300:
                    return
                if not response.headers.get(
                    "content-type", ""
                ).lower().startswith("application/json"):
                    raise RuntimeError("paid MCP response must be application/json")
                paid_response_body = await HttpxPayerMiddleware._read_bounded(
                    response, self.core.config.max_challenge_bytes
                )
                try:
                    paid_message = json.loads(paid_response_body)
                except (UnicodeDecodeError, ValueError) as error:
                    raise RuntimeError("paid MCP response is not valid JSON") from error
                if a2a is not None:
                    await asyncio.to_thread(a2a.report, operation, authorized, a2a_call, paid_message, response.headers)
                    return
                if not self.core._bound_mcp_response(
                    paid_message, request_id, require_result=True
                ):
                    raise RuntimeError(
                        "paid MCP response has no matching JSON-RPC result"
                    )
                meta = paid_message["result"].get("_meta", {})
                await asyncio.to_thread(
                    self.core.report_mcp, operation, authorized, meta
                )

            response = await self.core.deliver(
                operation,
                authorized,
                lambda _attempt: self._send(
                    method, url, request_headers, paid_body, stream=True, **kwargs
                ),
                lambda value: PayerHttpResponse(value.status_code, dict(value.headers)),
                lambda value: value.aclose(),
                report_mcp,
            )
            response.extensions["agentvault.payment"] = authorized
            if paid_response_body is not None:
                buffered = self._buffered(response, paid_response_body)
                await response.aclose()
                return buffered
            return response
        HttpxPayerMiddleware._event("agentvault.sdk.payer.payment_required", run_id=operation.run_id)
        origin = HttpxPayerMiddleware._canonical_origin(url)
        try:
            challenge_body = await HttpxPayerMiddleware._read_bounded(first, self.core.config.max_challenge_bytes)
        finally:
            await first.aclose()
        HttpxPayerMiddleware._event("agentvault.sdk.payer.signing_requested", run_id=operation.run_id)
        authorized = await asyncio.to_thread(
            self.core.authorize,
            PayerHttpRequest(method, url, origin, request_headers, HttpxPayerMiddleware._body_bytes(content), False),
            PayerHttpResponse(first.status_code, dict(first.headers), challenge_body),
            operation,
        )
        if authorized is None:
            raise RuntimeError("AgentVault did not authorize the 402 challenge")
        HttpxPayerMiddleware._event("agentvault.sdk.payer.signed", run_id=operation.run_id,
               intent_id=authorized.intent.id)
        name, value = self.core.credential_header(authorized, origin, request_headers)
        paid_headers = {**request_headers, name: value}
        response = await self.core.deliver(
            operation,
            authorized,
            lambda _attempt: self._send(method, url, paid_headers, content, **kwargs),
            lambda value: PayerHttpResponse(value.status_code, dict(value.headers)),
            lambda value: value.aclose(),
        )
        response.extensions["agentvault.payment"] = authorized
        return response

    async def get(self, url: str, **kwargs: Any) -> httpx.Response:
        return await self.request("GET", url, **kwargs)

    async def post(self, url: str, **kwargs: Any) -> httpx.Response:
        return await self.request("POST", url, **kwargs)

    async def _send(self, method: str, url: str, headers: Mapping[str, str],
                    content: bytes | str | None, *, stream: bool = False,
                    **kwargs: Any) -> httpx.Response:
        # This adapter owns the replayable body. HTTPX must frame its current
        # bytes, including after MCP/A2A metadata grows it and on every retry.
        headers = {name: value for name, value in headers.items()
                   if name.lower() not in {"content-length", "transfer-encoding"}}
        request = self._client.build_request(method, url, headers=headers, content=content, **kwargs)
        return await self._client.send(request, follow_redirects=False, stream=stream)

    def _mcp_request(
        self, method: str, headers: Mapping[str, str], body: bytes,
    ) -> dict[str, object] | None:
        content_type = next(
            (value for name, value in headers.items() if name.lower() == "content-type"),
            "",
        )
        if method.upper() != "POST" or not content_type.lower().startswith(
            "application/json"
        ):
            return None
        try:
            message = json.loads(body)
        except (UnicodeDecodeError, ValueError):
            return None
        if (
            not isinstance(message, dict)
            or message.get("jsonrpc") != "2.0"
            or message.get("method") != "tools/call"
            or not isinstance(message.get("params"), dict)
            or self.core._mcp_call_id(body) is None
        ):
            return None
        return message

    def _attach_mcp_credential(
        self, message: dict[str, object], key: str, value: object,
    ) -> bytes:
        copy = json.loads(json.dumps(message))
        params = copy["params"]
        assert isinstance(params, dict)
        root_meta = copy.get("_meta", {})
        if not isinstance(root_meta, dict):
            raise RuntimeError("JSON-RPC root _meta must be an object")
        if any(
            existing in root_meta
            for existing in self.core.mcp_root_request_metadata_keys()
        ):
            raise RuntimeError(
                "MCP request already contains root payment credentials"
            )
        meta = params.get("_meta", {})
        if not isinstance(meta, dict):
            raise RuntimeError("MCP params._meta must be an object")
        if any(
            existing in meta for existing in self.core.mcp_request_metadata_keys()
        ):
            raise RuntimeError("MCP request already contains payment credentials")
        meta[key] = value
        params["_meta"] = meta
        return json.dumps(copy, separators=(",", ":")).encode()

    @staticmethod
    def _buffered(response: httpx.Response, body: bytes) -> httpx.Response:
        return httpx.Response(
            response.status_code,
            headers=response.headers,
            content=body,
            request=response.request,
            extensions=response.extensions,
        )


    @staticmethod
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


    @staticmethod
    def _canonical_origin(url: str) -> str:
        parsed = urllib.parse.urlsplit(url)
        if parsed.scheme not in {"http", "https"} or not parsed.netloc or parsed.username:
            raise ValueError("paid request URL must be absolute HTTP(S) without userinfo")
        return urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, "", "", ""))


    @staticmethod
    def _body_bytes(value: bytes | str | None) -> bytes:
        if value is None:
            return b""
        return value if isinstance(value, bytes) else value.encode()


    @staticmethod
    def _event(name: str, **fields: object) -> None:
        rendered = " ".join(
            f"{key.replace('_', '.')}={value}" for key, value in sorted(fields.items())
        )
        message = f"{name} {rendered}" if rendered else name
        _LOG.info(message, extra={"event_name": name, **fields})
