from __future__ import annotations

import time
import urllib.parse
import re
from typing import Mapping

from ..config import PayerConfig
from ..model import (
    AuthorizationOutcome, AuthorizedPayment, AuthorizePaymentRequest, AuthorizePaymentResponse,
    BalanceQuery, OperationContext, PayerHttpRequest, PayerHttpResponse, PayerRequestContext,
    PaymentBalance, PaymentIntent, PaymentReceipt, PaymentRequired, ReportOutboundResponseRequest,
    ReportOutboundResponseResponse,
)
from ..transport import VaultTransport

_HEADER_NAME = re.compile(r"^[!#$%&'*+\-.^_`|~A-Za-z0-9]+$")
_RESERVED_HEADERS = {
    "host", "content-length", "authorization", "connection", "transfer-encoding", "upgrade",
    "proxy-authorization", "proxy-authenticate", "te", "trailer", "accept", "content-type",
}


class PayerMiddleware:
    """Transport-neutral 402/approval/signing/settlement state machine."""

    def __init__(self, transport: VaultTransport, config: PayerConfig) -> None:
        if transport is None:
            raise ValueError("AgentVault transport is not configured")
        self._transport = transport
        self.config = config

    def standard(self, *, timeout: float = 30.0,
              client: "httpx.AsyncClient | None" = None) -> "HttpxPayerMiddleware":
        from .standard import HttpxPayerMiddleware

        return HttpxPayerMiddleware(self, timeout=timeout, client=client)

    def authorize(self, request: PayerHttpRequest, response: PayerHttpResponse,
                  operation: PayerRequestContext) -> AuthorizedPayment | None:
        if response.status != 402:
            return None
        original = request
        _validate_request(original)
        if len(response.body) > self.config.max_challenge_bytes:
            raise RuntimeError("payment challenge exceeds configured limit")
        try:
            challenge_body = response.body.decode("utf-8", errors="strict")
        except UnicodeDecodeError as error:
            raise RuntimeError("payment challenge is not UTF-8") from error
        required = PaymentRequired(
            origin=request.origin,
            resource=request.url,
            content_type=_header(response.headers, "content-type") or "application/json",
            headers=dict(response.headers),
            body=challenge_body,
            protocol_hint=_header(response.headers, "payment-protocol") or None,
            expires_at=_header(response.headers, "payment-expires-at") or None,
        )
        if required.protocol_hint is not None and required.protocol_hint != self.config.protocol:
            raise RuntimeError("protocol must match the 402 protocol hint")
        authorization_request = AuthorizePaymentRequest(
            context=operation.operation_context(self.config.scenario_id),
            wallet_id=self.config.wallet_id,
            protocol=self.config.protocol,
            asset=self.config.asset,
            amount=self.config.amount,
            recipient=self.config.recipient,
            resource=original.url,
            challenge=required,
        )
        response = self._authorize_until_terminal(authorization_request)
        if response.authorization.outcome is AuthorizationOutcome.DENIED:
            raise PermissionError(f"AgentVault denied payment: {response.authorization.reason or 'policy denied'}")
        if response.authorization.outcome is not AuthorizationOutcome.AUTHORIZED or response.credential is None:
            raise RuntimeError("approval wait returned an invalid terminal result")
        credential = response.credential
        intent = response.intent
        if (
            not intent.id
            or intent.scenario_id != self.config.scenario_id
            or intent.run_id != operation.run_id
            or intent.wallet_id != self.config.wallet_id
            or intent.direction.value != "outbound"
            or intent.protocol != self.config.protocol
            or intent.protocol != credential.protocol
            or intent.asset != self.config.asset
            or intent.amount != self.config.amount
            or intent.recipient != self.config.recipient
            or intent.resource != original.url
            or intent.challenge_hash != credential.challenge_hash
            or intent.idempotency_key != operation.idempotency_key
        ):
            raise RuntimeError("credential, outbound intent, and paid resource are not bound to the same payment")
        return AuthorizedPayment(intent, credential)

    def report(self, operation: PayerRequestContext, payment: AuthorizedPayment,
               response: PayerHttpResponse) -> None:
        if payment.credential.protocol != "x402":
            return
        settlement = _header(response.headers, "payment-response")
        if settlement:
            self._report_outbound_response(
                operation.operation_context(self.config.scenario_id), payment.intent.id, settlement)
        elif 200 <= response.status < 300:
            raise RuntimeError(
                "successful x402 response has no PAYMENT-RESPONSE settlement hint; payment remains reserved")

    def credential_header(
        self, payment: AuthorizedPayment, origin: str, existing: Mapping[str, str],
    ) -> tuple[str, str]:
        delivery = payment.credential.http
        if delivery is None:
            raise RuntimeError("Vault credential has no HTTP delivery material")
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
        value = self._transport.request(
            "POST", "/api/v1/payments/authorize", request.wire(), request.context.idempotency_key)
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
                raise RuntimeError("idempotent authorization changed payment intent")
            if outcome is not AuthorizationOutcome.REQUIRES_APPROVAL:
                return response
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise TimeoutError("payment approval did not complete before the total timeout")
            time.sleep(min(self.config.approval_poll_interval, remaining))

    def _report_outbound_response(
        self, context: OperationContext, intent_id: str, payment_response: str,
    ) -> ReportOutboundResponseResponse:
        _required_query("intent_id", intent_id)
        request = ReportOutboundResponseRequest(context, payment_response)
        path = f"/api/v1/payments/{urllib.parse.quote(intent_id, safe='')}/outbound-response"
        value = self._transport.request("POST", path, request.wire(), context.idempotency_key)
        return ReportOutboundResponseResponse.from_wire(value)

    def get_payment(self, scenario_id: str, intent_id: str) -> PaymentIntent:
        _required_query("scenario_id", scenario_id)
        _required_query("intent_id", intent_id)
        query = urllib.parse.urlencode({"scenario_id": scenario_id})
        value = self._transport.request(
            "GET", f"/api/v1/payments/{urllib.parse.quote(intent_id, safe='')}?{query}")
        return PaymentIntent.from_wire(value)

    def get_receipt(self, scenario_id: str, intent_id: str) -> PaymentReceipt:
        _required_query("scenario_id", scenario_id)
        _required_query("intent_id", intent_id)
        query = urllib.parse.urlencode({"scenario_id": scenario_id})
        value = self._transport.request(
            "GET", f"/api/v1/payments/{urllib.parse.quote(intent_id, safe='')}/receipt?{query}")
        return PaymentReceipt.from_wire(value)

    def get_balance(self, query: BalanceQuery) -> PaymentBalance:
        params = {"scenario_id": query.scenario_id, "wallet_id": query.wallet_id, **query.asset.wire()}
        value = self._transport.request("GET", "/api/v1/payments/balance?" + urllib.parse.urlencode(params))
        return PaymentBalance.from_wire(value)


def _header(headers: Mapping[str, str], wanted: str) -> str:
    return next((value for name, value in headers.items() if name.lower() == wanted), "")


def _required_query(name: str, value: str) -> None:
    if not value.strip():
        raise ValueError(f"{name} cannot be blank")


def _validate_request(request: PayerHttpRequest) -> None:
    parsed = urllib.parse.urlsplit(request.url)
    origin = urllib.parse.urlunsplit((parsed.scheme, parsed.netloc, "", "", ""))
    if (
        request.follow_redirects or parsed.scheme not in {"http", "https"}
        or not parsed.netloc or parsed.username or origin.lower() != request.origin.lower()
    ):
        raise ValueError("paid request must use a canonical HTTP(S) origin with redirects disabled")
