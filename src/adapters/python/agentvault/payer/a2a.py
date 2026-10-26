from __future__ import annotations

from ..model import PayerHttpRequest, PayerHttpResponse
from ..model.a2a import A2ACall


class A2APayerMiddleware:
    """A2A carrier adapter; authorization, retries and reporting stay in the core."""

    def __init__(self, core) -> None:
        self.core = core

    def authorize(self, request: PayerHttpRequest, message: dict, operation, headers):
        call = A2ACall.parse(request.body, self.core.config.max_challenge_bytes)
        if call is None:
            return None
        A2ACall.response(message, call.envelope["id"])
        selected = self.core._protocols.a2a_challenge(message, self.core.config.max_challenge_bytes)
        if selected is None:
            return None
        adapter, challenge = selected
        normalized = {key.lower(): value for key, value in headers.items()}
        if "payment-required" in normalized or "www-authenticate" in normalized:
            raise ValueError("A2A challenge conflicts with HTTP authentication headers")
        if adapter.a2a_extension() and adapter.a2a_extension() not in {
            value.strip() for value in normalized.get("x-a2a-extensions", "").split(",")
        }:
            raise ValueError("A2A extension activation was not acknowledged")
        wire_headers = {**challenge.headers, "payment-protocol": adapter.name,
                        "content-type": challenge.content_type or "application/json"}
        if "payment-expires-at" in normalized:
            wire_headers["payment-expires-at"] = normalized["payment-expires-at"]
        return self.core.authorize(request, PayerHttpResponse(402, wire_headers, challenge.body), operation)

    def credential(self, payment, call: A2ACall, challenge: dict, origin: str) -> bytes:
        adapter = self.core._protocols.get(payment.credential.protocol)
        delivery = adapter.credential(payment.credential)
        if payment.intent.protocol != adapter.name or delivery.bound_origin.lower() != origin.lower():
            raise ValueError("A2A credential protocol/origin mismatch")
        _, existing = self.core._protocols.a2a_credential(call.envelope, self.core.config.max_challenge_bytes)
        if existing is not None:
            raise ValueError("A2A request already contains payment credentials")
        A2ACall.response(challenge, call.envelope["id"])
        if isinstance(challenge.get("result"), dict):
            call.bind_task(A2ACall.task(challenge))
        adapter.a2a_apply_credential(call.envelope, payment.credential)
        return call.body()

    def report(self, operation, payment, call: A2ACall, message: dict, headers) -> None:
        A2ACall.response(message, call.envelope["id"])
        if isinstance(message.get("result"), dict):
            call.bind_task(A2ACall.task(message))
        name, evidence = self.core._protocols.a2a_evidence(message, self.core.config.max_challenge_bytes)
        if evidence is None or name != payment.credential.protocol:
            raise ValueError("A2A response has no matching payment receipt; reservation remains pending")
        normalized = {key.lower(): value for key, value in headers.items()}
        existing = normalized.get(evidence.header_name.lower())
        if existing is not None and existing != evidence.header_value:
            raise ValueError("A2A receipt conflicts with HTTP receipt")
        self.core.report(operation, payment, PayerHttpResponse(200, {evidence.header_name: evidence.header_value}))
