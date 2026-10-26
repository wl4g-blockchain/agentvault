from __future__ import annotations

import asyncio
import json
import logging
import math
import time
import urllib.parse
import re
from typing import Awaitable, Callable, Mapping, TypeVar

from ..config import PayerConfig
from ..model import (
    AuthorizationOutcome, AuthorizedPayment, AuthorizePaymentRequest, AuthorizePaymentResponse,
    BalanceQuery, OperationContext, PayerHttpRequest, PayerHttpResponse, PayerRequestContext,
    PaymentBalance, PaymentIntent, PaymentReceipt, PaymentRequired, ReportOutboundResponseRequest,
    ReportOutboundResponseResponse, ProtocolEvidence,
)
from ..protocol import McpMetadata, PaymentProtocolFactory
from ..transport import VaultTransport

_HEADER_NAME = re.compile(r"^[!#$%&'*+\-.^_`|~A-Za-z0-9]+$")
_RESERVED_HEADERS = {
    "host", "content-length", "connection", "transfer-encoding", "upgrade",
    "proxy-authorization", "proxy-authenticate", "te", "trailer", "accept", "content-type",
}
_LOG = logging.getLogger("agentvault.sdk")
_ResponseT = TypeVar("_ResponseT")


class PayerMiddleware:
    """Transport-neutral 402/approval/signing/settlement state machine."""

    def __init__(self, transport: VaultTransport, config: PayerConfig) -> None:
        if transport is None:
            raise ValueError("AgentVault transport is not configured")
        self._transport = transport
        self.config = config
        self._protocols = PaymentProtocolFactory().create(config.protocols)

    def a2a(self) -> "A2APayerMiddleware":
        from .a2a import A2APayerMiddleware
        return A2APayerMiddleware(self)

    def standard(self, *, timeout: float = 30.0,
              client: "httpx.AsyncClient | None" = None) -> "HttpxPayerMiddleware":
        from .standard import HttpxPayerMiddleware

        return HttpxPayerMiddleware(self, timeout=timeout, client=client)

    def authorize(self, request: PayerHttpRequest, response: PayerHttpResponse,
                  operation: PayerRequestContext) -> AuthorizedPayment | None:
        if response.status != 402:
            return None
        original = request
        PayerMiddleware._validate_request(original)
        if len(response.body) > self.config.max_challenge_bytes:
            raise RuntimeError("payment challenge exceeds configured limit")
        try:
            challenge_body = response.body.decode("utf-8", errors="strict")
        except UnicodeDecodeError as error:
            raise RuntimeError("payment challenge is not UTF-8") from error
        required = PaymentRequired(
            origin=request.origin,
            resource=request.url,
            content_type=PayerMiddleware._header(response.headers, "content-type") or "application/json",
            headers=dict(response.headers),
            body=challenge_body,
            protocol_hint=PayerMiddleware._header(response.headers, "payment-protocol") or None,
            expires_at=PayerMiddleware._header(response.headers, "payment-expires-at") or None,
        )
        if required.protocol_hint is not None:
            self._protocols.get(required.protocol_hint)
        authorization_request = AuthorizePaymentRequest(
            context=operation.operation_context(self.config.scenario_id),
            challenge=required,
        )
        response = self._authorize_until_terminal(authorization_request)
        if response.authorization.outcome is AuthorizationOutcome.DENIED:
            raise PermissionError(f"AgentVault denied payment: {response.authorization.reason or 'policy denied'}")
        if response.authorization.outcome is not AuthorizationOutcome.AUTHORIZED or response.credential is None:
            raise RuntimeError("approval wait returned an invalid terminal result")
        credential = response.credential
        intent = response.intent
        self._protocols.get(intent.protocol)
        if (
            not intent.id
            or intent.scenario_id != self.config.scenario_id
            or intent.run_id != operation.run_id
            or not intent.wallet_id
            or not intent.policy_id
            or intent.direction.value != "outbound"
            or (required.protocol_hint is not None and intent.protocol != required.protocol_hint)
            or intent.protocol != credential.protocol
            or intent.resource != original.url
            or intent.challenge_hash != credential.challenge_hash
        ):
            raise RuntimeError("credential, outbound intent, and paid resource are not bound to the same payment")
        return AuthorizedPayment(intent, credential)

    def authorize_mcp(
        self,
        request: PayerHttpRequest,
        message: object,
        operation: PayerRequestContext,
        response_headers: Mapping[str, str] | None = None,
    ) -> AuthorizedPayment | None:
        request_id = self._mcp_call_id(request.body)
        if request_id is None or not self._bound_mcp_response(
            message, request_id, require_result=False
        ):
            return None
        selected = self._protocols.mcp_challenge(message, self.config.max_challenge_bytes)
        if selected is None:
            return None
        adapter, challenge = selected
        transport_headers = response_headers or {}
        if PayerMiddleware._header(transport_headers, "payment-required"):
            raise RuntimeError(
                "MCP payment challenge must not also declare PAYMENT-REQUIRED"
            )
        advertised = PayerMiddleware._header(transport_headers, "payment-protocol")
        if advertised and advertised.lower() != adapter.name.lower():
            raise RuntimeError(
                "MCP payment challenge protocol metadata conflicts with its JSON-RPC payload"
            )
        headers = dict(challenge.headers)
        headers["payment-protocol"] = adapter.name
        expires_at = PayerMiddleware._header(transport_headers, "payment-expires-at")
        if expires_at:
            headers["payment-expires-at"] = expires_at
        return self.authorize(
            request,
            PayerHttpResponse(402, headers, challenge.body),
            operation,
        )

    def _mcp_call_id(self, body: bytes) -> str | int | float | None:
        try:
            message = json.loads(body)
        except (UnicodeDecodeError, ValueError):
            return None
        if (
            not isinstance(message, dict)
            or message.get("jsonrpc") != "2.0"
            or message.get("method") != "tools/call"
            or not isinstance(message.get("params"), dict)
        ):
            return None
        call_id = message.get("id")
        if isinstance(call_id, bool) or not isinstance(call_id, (str, int, float)):
            return None
        if isinstance(call_id, float) and not math.isfinite(call_id):
            return None
        return call_id

    def _bound_mcp_response(
        self,
        message: object,
        expected_id: str | int | float,
        *,
        require_result: bool,
    ) -> bool:
        if not isinstance(message, dict) or message.get("jsonrpc") != "2.0":
            return False
        response_id = message.get("id")
        if type(response_id) is not type(expected_id) or response_id != expected_id:
            return False
        return not require_result or isinstance(message.get("result"), dict)

    def mcp_credential(self, payment: AuthorizedPayment, origin: str) -> McpMetadata:
        if payment.intent.protocol != payment.credential.protocol:
            raise RuntimeError("outbound payment protocol binding is invalid")
        adapter = self._protocols.get(payment.credential.protocol)
        delivery = adapter.credential(payment.credential)
        if delivery.bound_origin.lower() != origin.lower():
            raise RuntimeError("unsafe or missing MCP credential origin binding")
        return adapter.mcp_credential(payment.credential)

    def mcp_request_metadata_keys(self) -> tuple[str, ...]:
        return self._protocols.mcp_request_metadata_keys()

    def mcp_root_request_metadata_keys(self) -> tuple[str, ...]:
        return self._protocols.mcp_root_request_metadata_keys()

    def report(self, operation: PayerRequestContext, payment: AuthorizedPayment,
               response: PayerHttpResponse) -> None:
        if payment.intent.protocol != payment.credential.protocol:
            raise RuntimeError("outbound payment protocol binding is invalid")
        adapter = self._protocols.get(payment.credential.protocol)
        protocol_name, evidence = self._protocols.merchant_evidence(response.headers)
        if evidence is not None and protocol_name != adapter.name:
            raise RuntimeError(
                "HTTP settlement receipt protocol does not match the paid credential"
            )
        if evidence is not None:
            self._report_outbound_response(
                operation.operation_context(self.config.scenario_id), payment.intent.id,
                adapter.name, evidence.header_name, evidence.header_value)
        elif 200 <= response.status < 300:
            raise RuntimeError(
                f"successful {adapter.name} response has no settlement evidence; payment remains reserved")

    def report_mcp(
        self,
        operation: PayerRequestContext,
        payment: AuthorizedPayment,
        meta: object,
    ) -> None:
        if payment.intent.protocol != payment.credential.protocol:
            raise RuntimeError("outbound payment protocol binding is invalid")
        protocol_name, evidence = self._protocols.mcp_merchant_evidence(
            meta, self.config.max_challenge_bytes
        )
        if evidence is None or protocol_name != payment.intent.protocol:
            raise RuntimeError(
                f"successful {payment.intent.protocol} MCP response has no matching settlement evidence; "
                "payment remains reserved"
            )
        self._report_outbound_response(
            operation.operation_context(self.config.scenario_id),
            payment.intent.id,
            protocol_name,
            evidence.header_name,
            evidence.header_value,
        )

    async def deliver(
        self,
        operation: PayerRequestContext,
        payment: AuthorizedPayment,
        send: Callable[[int], Awaitable[_ResponseT]],
        view: Callable[[_ResponseT], PayerHttpResponse],
        discard: Callable[[_ResponseT], Awaitable[None]],
        report_response: Callable[[_ResponseT], Awaitable[None]] | None = None,
    ) -> _ResponseT:
        """Run the protocol-neutral bounded paid-request retry state machine."""
        for attempt in range(self.config.delivery_retries + 1):
            self._delivery_event(operation, payment, attempt + 1)
            response = await send(attempt + 1)
            response_view = view(response)
            try:
                if report_response is None:
                    await asyncio.to_thread(self.report, operation, payment, response_view)
                else:
                    await report_response(response)
            except BaseException:
                await discard(response)
                raise
            if response_view.status != 503 or attempt == self.config.delivery_retries:
                return response
            await discard(response)
        raise RuntimeError("payer retry loop ended unexpectedly")

    @staticmethod
    def _delivery_event(
        operation: PayerRequestContext, payment: AuthorizedPayment, attempt_no: int,
    ) -> None:
        _LOG.info(
            "agentvault.sdk.payer.retrying run.id=%s payment.intent.id=%s attempt.no=%s",
            operation.run_id, payment.intent.id, attempt_no,
            extra={
                "event_name": "agentvault.sdk.payer.retrying",
                "run_id": operation.run_id,
                "intent_id": payment.intent.id,
                "attempt_no": attempt_no,
            },
        )

    def credential_header(
        self, payment: AuthorizedPayment, origin: str, existing: Mapping[str, str],
    ) -> tuple[str, str]:
        adapter = self._protocols.get(payment.credential.protocol)
        delivery = adapter.credential(payment.credential)
        bound = delivery.bound_origin
        name = delivery.header_name
        value = delivery.header_value
        if (
            bound.lower() != origin.lower()
            or not _HEADER_NAME.fullmatch(name) or name.lower() in _RESERVED_HEADERS
            or "\r" in value or "\n" in value
            or any(header.lower() == name.lower() for header in existing)
        ):
            raise RuntimeError("unsafe or missing credential HTTP binding")
        return name, value

    def _authorize(self, request: AuthorizePaymentRequest) -> AuthorizePaymentResponse:
        value = self._transport.request("POST", "/api/v1/payments/authorize", request.wire())
        return AuthorizePaymentResponse.from_wire(value)

    def _authorize_until_terminal(self, request: AuthorizePaymentRequest) -> AuthorizePaymentResponse:
        deadline = time.monotonic() + self.config.approval_timeout
        expected_intent: str | None = None
        while True:
            response = self._authorize(request)
            outcome = response.authorization.outcome
            intent_id = response.authorization.intent_id
            if not intent_id or response.intent.id != intent_id:
                raise RuntimeError("invalid AgentVault authorization response")
            if expected_intent is None:
                expected_intent = intent_id
            elif intent_id != expected_intent:
                raise RuntimeError("replayed run changed payment intent")
            if outcome is not AuthorizationOutcome.REQUIRES_APPROVAL:
                return response
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("payment approval did not complete before the total timeout")
            time.sleep(min(self.config.approval_poll_interval, remaining))

    def _report_outbound_response(
        self, context: OperationContext, intent_id: str, protocol: str,
        header_name: str, header_value: str,
    ) -> ReportOutboundResponseResponse:
        PayerMiddleware._required_query("intent_id", intent_id)
        request = ReportOutboundResponseRequest(
            context, protocol=protocol,
            evidence=ProtocolEvidence(header_name, header_value),
        )
        path = f"/api/v1/payments/{urllib.parse.quote(intent_id, safe='')}/outbound-response"
        value = self._transport.request("POST", path, request.wire())
        return ReportOutboundResponseResponse.from_wire(value)

    def get_payment(self, scenario_id: str, intent_id: str) -> PaymentIntent:
        PayerMiddleware._required_query("scenario_id", scenario_id)
        PayerMiddleware._required_query("intent_id", intent_id)
        query = urllib.parse.urlencode({"scenario_id": scenario_id})
        value = self._transport.request(
            "GET", f"/api/v1/payments/{urllib.parse.quote(intent_id, safe='')}?{query}")
        return PaymentIntent.from_wire(value)

    def get_receipt(self, scenario_id: str, intent_id: str) -> PaymentReceipt:
        PayerMiddleware._required_query("scenario_id", scenario_id)
        PayerMiddleware._required_query("intent_id", intent_id)
        query = urllib.parse.urlencode({"scenario_id": scenario_id})
        value = self._transport.request(
            "GET", f"/api/v1/payments/{urllib.parse.quote(intent_id, safe='')}/receipt?{query}")
        return PaymentReceipt.from_wire(value)

    def get_balance(self, query: BalanceQuery) -> PaymentBalance:
        params = {"scenario_id": query.scenario_id, "wallet_id": query.wallet_id, **query.asset.wire()}
        value = self._transport.request("GET", "/api/v1/payments/balance?" + urllib.parse.urlencode(params))
        return PaymentBalance.from_wire(value)


    @staticmethod
    def _header(headers: Mapping[str, str], wanted: str) -> str:
        return next((value for name, value in headers.items() if name.lower() == wanted), "")


    @staticmethod
    def _required_query(name: str, value: str) -> None:
        if not value.strip():
            raise ValueError(f"{name} cannot be blank")


    @staticmethod
    def _validate_request(request: PayerHttpRequest) -> None:
        parsed = urllib.parse.urlsplit(request.url)
        origin = urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, "", "", ""))
        if (
            request.follow_redirects or parsed.scheme not in {"http", "https"}
            or not parsed.netloc or parsed.username or origin.lower() != request.origin.lower()
        ):
            raise ValueError("paid request must use a canonical HTTP(S) origin with redirects disabled")
