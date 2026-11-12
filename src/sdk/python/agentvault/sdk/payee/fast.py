from __future__ import annotations

import asyncio
import hashlib
import json
import math
from dataclasses import dataclass, replace
from typing import Any, Awaitable, Callable, Mapping, MutableMapping

from ..model import PayeeOperation, PaymentChallenge, SettledIncomingPayment
from ..protocol import PaymentProtocol
from ..transport import APIError
from a2a.server.tasks import TaskStore
from .a2a import A2APaymentCall

from .payee import PayeeMiddleware, PaymentRejectedError

ASGIApp = Callable[
    [
        MutableMapping[str, Any],
        Callable[[], Awaitable[Mapping[str, Any]]],
        Callable[[Mapping[str, Any]], Awaitable[None]],
    ],
    Awaitable[None],
]


@dataclass(frozen=True)
class FastPayeeRoute:
    method: str
    path: str
    scenario_id: str
    price_id: str
    resource: str
    origin: str
    protocol: str = "x402"
    mcp_tool_name: str | None = None
    a2a_task_store: TaskStore | None = None

    @classmethod
    def mcp_tool(
        cls,
        path: str,
        tool: str,
        scenario_id: str,
        price_id: str,
        resource: str,
        origin: str,
    ) -> "FastPayeeRoute":
        return cls.mcp_tool_with_protocol(
            path, tool, scenario_id, price_id, resource, origin, "x402"
        )

    @classmethod
    def mcp_tool_with_protocol(
        cls,
        path: str,
        tool: str,
        scenario_id: str,
        price_id: str,
        resource: str,
        origin: str,
        protocol: str,
    ) -> "FastPayeeRoute":
        return cls("POST", path, scenario_id, price_id, resource, origin, protocol, tool)


@dataclass(frozen=True)
class _McpCall:
    id: str | int
    credential: object | None
    canonical_body: bytes


class _InvalidPaidResponse(RuntimeError):
    pass


class FastPayeeMiddleware:
    """Pure ASGI middleware, directly compatible with FastAPI.add_middleware."""

    def __init__(
        self,
        app: ASGIApp,
        *,
        middleware: PayeeMiddleware,
        routes: list[FastPayeeRoute],
        max_body_bytes: int = 1024 * 1024,
    ) -> None:
        if middleware is None or max_body_bytes <= 0:
            raise ValueError("payee middleware and body limit are required")
        self.app = app
        self.middleware = middleware
        if any(route.mcp_tool_name and route.a2a_task_store for route in routes):
            raise ValueError("MCP and A2A routes are mutually exclusive")
        self.routes = {(route.method.upper(), route.path): route for route in routes}
        self.max_body_bytes = max_body_bytes

    async def __call__(
        self,
        scope: MutableMapping[str, Any],
        receive: Callable[[], Awaitable[Mapping[str, Any]]],
        send: Callable[[Mapping[str, Any]], Awaitable[None]],
    ) -> None:
        if scope.get("type") != "http":
            await self.app(scope, receive, send)
            return
        route = self.routes.get(
            (str(scope.get("method", "")).upper(), str(scope.get("path", "")))
        )
        if route is None:
            await self.app(scope, receive, send)
            return
        try:
            body = await self._read_body(receive)
        except ValueError as error:
            await self._error(send, 413, "REQUEST_TOO_LARGE", str(error))
            return
        try:
            headers = self._headers(scope.get("headers", []))
        except ValueError as error:
            await self._error(send, 400, "PAYMENT_INVALID", str(error))
            return
        try:
            call = (
                self._mcp_call(body, route)
                if route.mcp_tool_name is not None
                else None
            )
        except ValueError as error:
            await self._error(send, 400, "PAYMENT_INVALID", str(error))
            return
        if route.mcp_tool_name is not None and call is None:
            await self.app(scope, self._body_receiver(body), send)
            return
        run_id = headers.get("x-run-id", "").strip()
        if not run_id:
            await self._error(send, 400, "PAYMENT_CONTEXT_REQUIRED", "x-run-id is required")
            return
        try:
            operation = PayeeOperation(
                route.scenario_id, run_id, route.price_id, route.resource,
                route.origin, route.protocol, self._request_hash(route.method, route.path, body),
            )
            if route.a2a_task_store is not None:
                call = await self.middleware.a2a().prepare(body, operation, route.a2a_task_store, headers, scope.get("state", {}).get("a2a_context"))
                if call is None:
                    await self.app(scope, self._body_receiver(body), send)
                    return
            adapter = self.middleware.protocol(route.protocol)
            http_credential = self.middleware.presented_credential(
                route.protocol, headers
            )
            if call is not None and http_credential is not None:
                raise ValueError(
                    "MCP payment credentials must use protocol metadata, not HTTP headers"
                )
            credential = (
                call.credential
                if call is not None
                else http_credential
            )
            hash_body = call.canonical_body if call is not None else body
            operation = replace(
                operation,
                request_hash=self._request_hash(route.method, route.path, hash_body),
                credential_payload=credential,
            )
            outcome = await asyncio.to_thread(self.middleware.protect, operation)
        except PaymentRejectedError as error:
            if call is not None:
                await self._send_mcp_challenge(send, call, error.challenge, True)
            else:
                await self._send_http_challenge(send, adapter, error.challenge)
            return
        except APIError as error:
            await self._error(send, error.status, error.code, str(error))
            return
        except ValueError as error:
            await self._error(send, 400, "PAYMENT_INVALID", str(error))
            return
        except Exception as error:
            await self._error(send, 502, "AGENTVAULT_UNAVAILABLE", str(error))
            return
        if outcome.outcome == "payment_required" and outcome.challenge is not None:
            if call is not None:
                await self._send_mcp_challenge(send, call, outcome.challenge, False)
            else:
                await self._send_http_challenge(send, adapter, outcome.challenge)
            return
        payment = outcome.payment
        if outcome.outcome != "authorized" or payment is None:
            await self._error(
                send,
                502,
                "SETTLEMENT_INVALID",
                "AgentVault returned an invalid protection outcome",
            )
            return
        state = scope.setdefault("state", {})
        state["agentvault_payment"] = payment
        if call is not None:
            if outcome.challenge is None:
                await self._error(
                    send, 502, "SETTLEMENT_INVALID", "MCP settlement omitted its challenge"
                )
                return
            released = await self._serve_mcp_result(
                scope, call.canonical_body, send, call, outcome.challenge, payment
            )
        else:
            released = await self._serve_http_result(
                scope, body, send, adapter, payment
            )
        if released:
            PayeeMiddleware._event(
                "agentvault.sdk.payee.resource_released",
                run_id=run_id,
                intent_id=payment.intent.id,
            )

    def _mcp_call(self, body: bytes, route: FastPayeeRoute) -> _McpCall | None:
        try:
            message = json.loads(body)
        except (UnicodeDecodeError, ValueError):
            return None
        if (
            not isinstance(message, dict)
            or message.get("jsonrpc") != "2.0"
            or message.get("method") != "tools/call"
            or not isinstance(message.get("params"), dict)
            or message["params"].get("name") != route.mcp_tool_name
        ):
            return None
        call_id = message.get("id")
        if (
            not isinstance(call_id, (str, int, float))
            or isinstance(call_id, bool)
            or (isinstance(call_id, float) and not math.isfinite(call_id))
        ):
            raise ValueError("paid MCP tools/call requires a string or numeric JSON-RPC id")
        nested_meta = message["params"].get("_meta", {})
        if not isinstance(nested_meta, dict):
            raise ValueError("MCP params._meta must be an object")
        meta = dict(nested_meta)
        root_meta = message.get("_meta", {})
        if not isinstance(root_meta, dict):
            raise ValueError("JSON-RPC root _meta must be an object")
        for key in self.middleware.mcp_root_request_metadata_keys():
            if key in root_meta:
                if key in meta:
                    raise ValueError("duplicate MCP payment credential metadata")
                meta[key] = root_meta[key]
        credential = self.middleware.presented_mcp_credential(route.protocol, meta)
        for key in self.middleware.mcp_request_metadata_keys():
            nested_meta.pop(key, None)
        if nested_meta:
            message["params"]["_meta"] = nested_meta
        else:
            message["params"].pop("_meta", None)
        for key in self.middleware.mcp_root_request_metadata_keys():
            root_meta.pop(key, None)
        if "_meta" in message and not root_meta:
            message.pop("_meta")
        canonical = json.dumps(
            message, separators=(",", ":"), sort_keys=True
        ).encode()
        return _McpCall(call_id, credential, canonical)

    async def _send_http_challenge(
        self,
        send: Callable[[Mapping[str, Any]], Awaitable[None]],
        adapter: PaymentProtocol,
        challenge: PaymentChallenge,
    ) -> None:
        try:
            wire = adapter.challenge(challenge)
            headers = [
                (name.lower().encode("ascii"), value.encode("ascii"))
                for name, value in wire.headers.items()
            ]
            headers.extend([
                (b"content-type", (wire.content_type or "application/json").encode("ascii")),
                (b"payment-protocol", challenge.protocol.encode("ascii")),
                (b"payment-expires-at", str(challenge.expires_at).encode("ascii")),
            ])
        except (UnicodeError, ValueError) as error:
            await self._error(send, 502, "CHALLENGE_INVALID", str(error))
            return
        await send({"type": "http.response.start", "status": 402, "headers": headers})
        await send({"type": "http.response.body", "body": wire.body})

    async def _send_mcp_challenge(
        self,
        send: Callable[[Mapping[str, Any]], Awaitable[None]],
        call: _McpCall,
        challenge: PaymentChallenge,
        rejected: bool,
    ) -> None:
        try:
            wire = (await self.middleware.a2a().challenge(call, challenge, rejected)
                    if isinstance(call, A2APaymentCall) else self.middleware.mcp_payment_required(challenge, rejected))
            message: dict[str, object] = {"jsonrpc": "2.0", "id": call.id}
            if wire.result is not None:
                message["result"] = wire.result
            elif wire.error is not None:
                message["error"] = wire.error
            else:
                raise ValueError("protocol returned no MCP challenge")
            body = json.dumps(message, separators=(",", ":")).encode()
            if len(body) > self.max_body_bytes:
                raise ValueError("MCP payment challenge exceeds configured limit")
        except (TypeError, ValueError) as error:
            await self._error(send, 502, "CHALLENGE_INVALID", str(error))
            return
        await send({
            "type": "http.response.start",
            "status": 200,
            "headers": [
                (b"content-type", b"application/json"),
                (b"payment-protocol", challenge.protocol.encode("ascii")),
                (b"payment-expires-at", str(challenge.expires_at).encode("ascii")),
                *([(b"x-a2a-extensions", self.middleware.protocol(challenge.protocol).a2a_extension().encode("ascii"))]
                  if isinstance(call, A2APaymentCall) and self.middleware.protocol(challenge.protocol).a2a_extension() else []),
            ],
        })
        await send({"type": "http.response.body", "body": body})

    async def _serve_http_result(
        self,
        scope: MutableMapping[str, Any],
        body: bytes,
        send: Callable[[Mapping[str, Any]], Awaitable[None]],
        adapter: PaymentProtocol,
        payment: SettledIncomingPayment,
    ) -> bool:
        try:
            receipt = adapter.receipt(payment)
        except ValueError as error:
            await self._error(send, 502, "SETTLEMENT_EVIDENCE_INVALID", str(error))
            return False

        released = False
        started = False

        async def paid_send(message: Mapping[str, Any]) -> None:
            nonlocal released, started
            if message.get("type") != "http.response.start":
                await send(message)
                return
            mutable = dict(message)
            response_headers = list(mutable.get("headers", []))
            existing = {
                bytes(name).decode("latin-1").lower(): bytes(value).decode("latin-1")
                for name, value in response_headers
            }
            if any(
                name.lower() in existing
                for name in self.middleware.merchant_evidence_header_names()
            ):
                raise _InvalidPaidResponse(
                    "paid handler attempted to supply settlement evidence"
                )
            try:
                response_headers.extend(
                    (name.lower().encode("ascii"), value.encode("ascii"))
                    for name, value in receipt.headers.items()
                )
            except UnicodeError as error:
                raise _InvalidPaidResponse(
                    "settlement evidence contains an invalid HTTP header"
                ) from error
            mutable["headers"] = response_headers
            await send(mutable)
            started = True
            released = 200 <= int(mutable.get("status", 0)) < 300

        try:
            await self.app(scope, self._body_receiver(body), paid_send)
        except _InvalidPaidResponse as error:
            if not started:
                await self._error(
                    send, 502, "SETTLEMENT_EVIDENCE_INVALID", str(error)
                )
            return False
        return released

    async def _serve_mcp_result(
        self,
        scope: MutableMapping[str, Any],
        request_body: bytes,
        send: Callable[[Mapping[str, Any]], Awaitable[None]],
        call: _McpCall,
        challenge: PaymentChallenge,
        payment: SettledIncomingPayment,
    ) -> bool:
        captured: list[Mapping[str, Any]] = []
        size = 0

        async def capture(message: Mapping[str, Any]) -> None:
            nonlocal size
            if message.get("type") == "http.response.body":
                chunk = message.get("body", b"")
                if not isinstance(chunk, bytes):
                    raise RuntimeError("paid MCP handler returned an invalid body")
                size += len(chunk)
                if size > self.max_body_bytes:
                    raise RuntimeError("paid MCP response exceeds configured limit")
                if message.get("more_body") is True:
                    raise RuntimeError("streaming paid MCP responses are not replayable")
            captured.append(message)

        try:
            await self.app(scope, self._body_receiver(request_body), capture)
        except RuntimeError as error:
            await self._error(send, 502, "MCP_RESPONSE_INVALID", str(error))
            return False
        starts = [message for message in captured if message.get("type") == "http.response.start"]
        bodies = [message for message in captured if message.get("type") == "http.response.body"]
        if len(starts) != 1 or len(bodies) != 1:
            await self._error(
                send, 502, "MCP_RESPONSE_INVALID", "paid MCP handler returned an invalid response"
            )
            return False
        status = int(starts[0].get("status", 0))
        response_headers = list(starts[0].get("headers", []))
        headers = self._headers(response_headers)
        if not headers.get("content-type", "").lower().startswith("application/json"):
            await self._error(
                send, 502, "MCP_RESPONSE_INVALID", "paid MCP handler must return application/json"
            )
            return False
        if any(
            name.lower() in headers
            for name in self.middleware.merchant_evidence_header_names()
        ):
            await self._error(
                send, 502, "MCP_RESPONSE_INVALID", "paid MCP handler supplied settlement evidence"
            )
            return False
        try:
            message = json.loads(bodies[0].get("body", b""))
            if not isinstance(message, dict) or message.get("id") != call.id:
                raise ValueError("paid MCP response id does not match its request")
            if isinstance(call, A2APaymentCall):
                message = await self.middleware.a2a().receipt(call, message, challenge, payment)
            else:
                result = message.get("result")
                if result is not None and not isinstance(result, dict):
                    raise ValueError("paid MCP handler returned an invalid JSON-RPC result")
                meta = result.get("_meta", {}) if isinstance(result, dict) else {}
                if not isinstance(meta, dict):
                    raise ValueError("paid MCP result._meta must be an object")
                if isinstance(result, dict):
                    protocol_name, evidence = self.middleware.mcp_merchant_evidence(meta)
                    if evidence is not None:
                        raise ValueError(
                            f"paid MCP handler attempted to supply {protocol_name} settlement metadata"
                        )
                if not 200 <= status < 300:
                    if result is None and not isinstance(message.get("error"), dict):
                        raise ValueError("paid MCP handler must return a JSON-RPC result or error")
                    await send(starts[0])
                    await send(bodies[0])
                    return False
                if not isinstance(result, dict):
                    raise ValueError("paid MCP handler must return a JSON-RPC result")
                receipt = self.middleware.mcp_receipt(challenge, payment)
                if receipt.key in meta:
                    raise ValueError("paid MCP handler attempted to supply settlement metadata")
                meta[receipt.key] = receipt.value
                result["_meta"] = meta
            encoded = json.dumps(message, separators=(",", ":")).encode()
            if len(encoded) > self.max_body_bytes:
                raise ValueError("paid MCP response exceeds configured limit")
        except (TypeError, ValueError) as error:
            await self._error(send, 502, "MCP_RESPONSE_INVALID", str(error))
            return False
        filtered_headers = [
            (name, value)
            for name, value in response_headers
            if bytes(name).decode("latin-1").lower() != "content-length"
        ]
        await send({
            "type": "http.response.start",
            "status": int(starts[0]["status"]),
            "headers": filtered_headers,
        })
        await send({"type": "http.response.body", "body": encoded})
        return True

    def _headers(self, raw: object) -> dict[str, str]:
        result: dict[str, str] = {}
        payment_headers = {
            name.lower() for name in self.middleware.credential_header_names()
        }
        for name, value in raw if isinstance(raw, list) else []:
            decoded_name = bytes(name).decode("latin-1").lower()
            if decoded_name in result and decoded_name in payment_headers:
                raise ValueError(
                    f"duplicate payment request header is not allowed: {decoded_name}"
                )
            result[decoded_name] = bytes(value).decode("latin-1")
        return result

    async def _read_body(
        self, receive: Callable[[], Awaitable[Mapping[str, Any]]],
    ) -> bytes:
        body = bytearray()
        while True:
            message = await receive()
            if message.get("type") != "http.request":
                continue
            chunk = message.get("body", b"")
            if not isinstance(chunk, bytes):
                raise ValueError("invalid ASGI request body")
            body.extend(chunk)
            if len(body) > self.max_body_bytes:
                raise ValueError("protected request body exceeds configured limit")
            if not message.get("more_body", False):
                return bytes(body)

    @staticmethod
    def _body_receiver(
        body: bytes,
    ) -> Callable[[], Awaitable[Mapping[str, Any]]]:
        delivered = False

        async def receive() -> Mapping[str, Any]:
            nonlocal delivered
            if delivered:
                return {"type": "http.request", "body": b"", "more_body": False}
            delivered = True
            return {"type": "http.request", "body": body, "more_body": False}

        return receive

    @staticmethod
    def _request_hash(method: str, path: str, body: bytes) -> str:
        digest = hashlib.sha256()
        digest.update(b"agentvault.payee.http.v1\0")
        digest.update(method.upper().encode())
        digest.update(b"\0")
        digest.update(path.encode())
        digest.update(b"\0")
        digest.update(body)
        return digest.hexdigest()

    @staticmethod
    async def _error(
        send: Callable[[Mapping[str, Any]], Awaitable[None]],
        status: int,
        code: str,
        message: str,
    ) -> None:
        body = json.dumps(
            {"error": {"code": code, "message": message}}, separators=(",", ":")
        ).encode()
        await send({
            "type": "http.response.start",
            "status": status,
            "headers": [(b"content-type", b"application/json")],
        })
        await send({"type": "http.response.body", "body": body})


    @staticmethod
    def settled_payment(scope: Mapping[str, Any]) -> SettledIncomingPayment | None:
        state = scope.get("state")
        value = state.get("agentvault_payment") if isinstance(state, dict) else None
        return value if isinstance(value, SettledIncomingPayment) else None
