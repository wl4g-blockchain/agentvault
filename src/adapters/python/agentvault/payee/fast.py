from __future__ import annotations

import asyncio
import base64
import hashlib
import json
from dataclasses import dataclass
from typing import Any, Awaitable, Callable, Mapping, MutableMapping

from ..model import PayeeOperation, PaymentChallenge, SettledIncomingPayment
from ..transport import APIError
from .payee import PayeeMiddleware

ASGIApp = Callable[[MutableMapping[str, Any], Callable[[], Awaitable[Mapping[str, Any]]],
                    Callable[[Mapping[str, Any]], Awaitable[None]]], Awaitable[None]]


@dataclass(frozen=True)
class FastPayeeRoute:
    method: str
    path: str
    scenario_id: str
    offer_id: str
    resource: str
    origin: str
    protocol: str = "x402"
    mcp_tool: str | None = None

    @classmethod
    def mcp_tool(cls, path: str, tool: str, scenario_id: str, offer_id: str,
                 resource: str, origin: str) -> "FastPayeeRoute":
        return cls("POST", path, scenario_id, offer_id, resource, origin, "x402", tool)


class FastPayeeMiddleware:
    """Pure ASGI middleware, directly compatible with FastAPI.add_middleware."""

    def __init__(self, app: ASGIApp, *, middleware: PayeeMiddleware,
                 routes: list[FastPayeeRoute], max_body_bytes: int = 1024 * 1024) -> None:
        if middleware is None or max_body_bytes <= 0:
            raise ValueError("payee middleware and body limit are required")
        self.app = app
        self.middleware = middleware
        self.routes = {(route.method.upper(), route.path): route for route in routes}
        self.max_body_bytes = max_body_bytes

    async def __call__(self, scope: MutableMapping[str, Any], receive: Callable[[], Awaitable[Mapping[str, Any]]],
                       send: Callable[[Mapping[str, Any]], Awaitable[None]]) -> None:
        if scope.get("type") != "http":
            await self.app(scope, receive, send)
            return
        route = self.routes.get((str(scope.get("method", "")).upper(), str(scope.get("path", ""))))
        if route is None:
            await self.app(scope, receive, send)
            return
        try:
            body = await _read_body(receive, self.max_body_bytes)
        except ValueError as error:
            await _error(send, 413, "REQUEST_TOO_LARGE", str(error))
            return
        if route.mcp_tool is not None and not _is_mcp_tool_call(body, route.mcp_tool):
            await self.app(scope, _body_receiver(body), send)
            return
        headers = _headers(scope.get("headers", []))
        run_id = headers.get("x-run-id", "").strip()
        if not run_id:
            await _error(send, 400, "PAYMENT_CONTEXT_REQUIRED", "x-run-id is required")
            return
        try:
            credential = _decode_credential(headers.get("payment-signature"), self.max_body_bytes)
            operation = PayeeOperation(
                route.scenario_id, run_id, route.offer_id, route.resource,
                route.origin, route.protocol, _request_hash(route.method, route.path, body), credential)
            outcome = await asyncio.to_thread(self.middleware.protect, operation)
        except PermissionError as error:
            await _error(send, 402, "PAYMENT_INVALID", str(error))
            return
        except APIError as error:
            await _error(send, error.status, error.code, str(error))
            return
        except ValueError as error:
            await _error(send, 400, "PAYMENT_INVALID", str(error))
            return
        except Exception as error:
            await _error(send, 502, "AGENTVAULT_UNAVAILABLE", str(error))
            return
        if outcome.outcome == "payment_required" and outcome.challenge is not None:
            await _challenge(send, outcome.challenge)
            return
        payment = outcome.payment
        if outcome.outcome != "authorized" or payment is None:
            await _error(send, 502, "SETTLEMENT_INVALID", "AgentVault returned an invalid protection outcome")
            return
        try:
            payment_response = _payment_response(payment)
        except ValueError as error:
            await _error(send, 502, "SETTLEMENT_EVIDENCE_INVALID", str(error))
            return
        state = scope.setdefault("state", {})
        state["agentvault_payment"] = payment

        async def paid_send(message: Mapping[str, Any]) -> None:
            if message.get("type") == "http.response.start":
                mutable = dict(message)
                response_headers = list(mutable.get("headers", []))
                response_headers.append((b"payment-response", payment_response.encode("ascii")))
                mutable["headers"] = response_headers
                await send(mutable)
            else:
                await send(message)

        await self.app(scope, _body_receiver(body), paid_send)


def settled_payment(scope: Mapping[str, Any]) -> SettledIncomingPayment | None:
    state = scope.get("state")
    value = state.get("agentvault_payment") if isinstance(state, dict) else None
    return value if isinstance(value, SettledIncomingPayment) else None


def _headers(raw: Any) -> dict[str, str]:
    result: dict[str, str] = {}
    for name, value in raw if isinstance(raw, list) else []:
        result[bytes(name).decode("latin-1").lower()] = bytes(value).decode("latin-1")
    return result


async def _read_body(receive: Callable[[], Awaitable[Mapping[str, Any]]], limit: int) -> bytes:
    body = bytearray()
    while True:
        message = await receive()
        if message.get("type") != "http.request":
            continue
        chunk = message.get("body", b"")
        if not isinstance(chunk, bytes):
            raise ValueError("invalid ASGI request body")
        body.extend(chunk)
        if len(body) > limit:
            raise ValueError("protected request body exceeds configured limit")
        if not message.get("more_body", False):
            return bytes(body)


def _body_receiver(body: bytes) -> Callable[[], Awaitable[Mapping[str, Any]]]:
    delivered = False

    async def receive() -> Mapping[str, Any]:
        nonlocal delivered
        if delivered:
            return {"type": "http.request", "body": b"", "more_body": False}
        delivered = True
        return {"type": "http.request", "body": body, "more_body": False}

    return receive


def _is_mcp_tool_call(body: bytes, expected: str) -> bool:
    try:
        value = json.loads(body)
    except (UnicodeDecodeError, ValueError):
        return False
    return (
        isinstance(value, dict)
        and value.get("method") == "tools/call"
        and isinstance(value.get("params"), dict)
        and value["params"].get("name") == expected
    )


def _decode_credential(encoded: str | None, limit: int) -> Any | None:
    if not encoded:
        return None
    if len(encoded) > (limit * 4 + 2) // 3 + 4:
        raise ValueError("PAYMENT-SIGNATURE exceeds configured limit")
    try:
        raw = base64.b64decode(encoded, validate=True)
    except ValueError as error:
        raise ValueError("PAYMENT-SIGNATURE is not canonical base64") from error
    if len(raw) > limit or base64.b64encode(raw).decode("ascii") != encoded:
        raise ValueError("PAYMENT-SIGNATURE is non-canonical or oversized")
    try:
        payload = json.loads(raw)
    except (UnicodeDecodeError, ValueError) as error:
        raise ValueError("PAYMENT-SIGNATURE is not valid x402 JSON") from error
    if not isinstance(payload, dict):
        raise ValueError("PAYMENT-SIGNATURE must be an x402 JSON object")
    return payload


def _request_hash(method: str, path: str, body: bytes) -> str:
    digest = hashlib.sha256()
    digest.update(b"agentvault.payee.http.v1\0")
    digest.update(method.upper().encode())
    digest.update(b"\0")
    digest.update(path.encode())
    digest.update(b"\0")
    digest.update(body)
    return digest.hexdigest()


async def _challenge(
    send: Callable[[Mapping[str, Any]], Awaitable[None]], challenge: PaymentChallenge,
) -> None:
    if not isinstance(challenge.payload, dict):
        await _error(send, 502, "CHALLENGE_INVALID", "AgentVault challenge payload is invalid")
        return
    required = challenge.payload.get("payment_required")
    if required is None:
        await _error(send, 502, "CHALLENGE_INVALID", "AgentVault returned no protocol wire challenge")
        return
    body = json.dumps(required, separators=(",", ":")).encode()
    headers = [
        (b"content-type", b"application/json"),
        (b"payment-protocol", challenge.protocol.encode()),
        (b"payment-expires-at", str(challenge.expires_at).encode()),
        (b"payment-required", base64.b64encode(body)),
    ]
    await send({"type": "http.response.start", "status": 402, "headers": headers})
    await send({"type": "http.response.body", "body": body})


def _payment_response(payment: SettledIncomingPayment) -> str:
    receipt = payment.receipt
    if not receipt.tx_hash or not receipt.payer:
        raise ValueError("confirmed x402 receipt omitted transaction or payer")
    if not receipt.asset.network:
        raise ValueError("confirmed x402 receipt omitted network")
    body = json.dumps({"success": True, "payer": receipt.payer,
                       "transaction": receipt.tx_hash, "network": receipt.asset.network},
                      separators=(",", ":")).encode()
    return base64.b64encode(body).decode("ascii")


async def _error(send: Callable[[Mapping[str, Any]], Awaitable[None]], status: int,
                 code: str, message: str) -> None:
    body = json.dumps({"error": {"code": code, "message": message}}, separators=(",", ":")).encode()
    await send({"type": "http.response.start", "status": status,
                "headers": [(b"content-type", b"application/json")]})
    await send({"type": "http.response.body", "body": body})
