from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Mapping

from .approval import ApprovalRequest
from .base import (
    AssetIdentity,
    OperationContext,
    PaymentCredential,
    PaymentIntent,
    PaymentReceipt,
    required,
)


class AuthorizationOutcome(str, Enum):
    AUTHORIZED = "authorized"
    REQUIRES_APPROVAL = "requires_approval"
    DENIED = "denied"


@dataclass(frozen=True)
class PaymentRequired:
    origin: str
    resource: str
    content_type: str
    body: str
    protocol_hint: str | None = None
    headers: Mapping[str, str] = field(default_factory=dict)
    expires_at: str | None = None

    def wire(self) -> dict[str, Any]:
        value: dict[str, Any] = {
            "origin": self.origin,
            "resource": self.resource,
            "content_type": self.content_type,
            "headers": dict(self.headers),
            "body": self.body,
        }
        if self.protocol_hint is not None:
            value["protocol_hint"] = self.protocol_hint
        if self.expires_at is not None:
            value["expires_at"] = self.expires_at
        return value

    def __repr__(self) -> str:
        return f"PaymentRequired(origin={self.origin!r}, resource=<redacted>, headers=<redacted>, body=<redacted>)"


@dataclass(frozen=True)
class AuthorizePaymentRequest:
    context: OperationContext
    wallet_id: str
    protocol: str
    asset: AssetIdentity
    amount: str
    recipient: str
    resource: str
    challenge: PaymentRequired

    def __post_init__(self) -> None:
        if self.context.run_id is None:
            raise ValueError("run_id is required for payment operations")
        for name, value in (
            ("wallet_id", self.wallet_id), ("protocol", self.protocol), ("amount", self.amount),
            ("recipient", self.recipient), ("resource", self.resource),
        ):
            required(name, value)
        if self.resource != self.challenge.resource:
            raise ValueError("resource must exactly match the challenge resource")
        if self.challenge.protocol_hint is not None and self.challenge.protocol_hint != self.protocol:
            raise ValueError("protocol must match the 402 protocol hint")

    def wire(self) -> dict[str, Any]:
        return {
            **self.context.wire(),
            "wallet_id": self.wallet_id,
            "protocol": self.protocol,
            "asset": self.asset.wire(),
            "amount": self.amount,
            "recipient": self.recipient,
            "resource": self.resource,
            "challenge": self.challenge.wire(),
        }


@dataclass(frozen=True)
class PaymentAuthorizationResult:
    outcome: AuthorizationOutcome
    intent_id: str
    approval_id: str | None = None
    reserved_amount: str | None = None
    reason: str | None = None

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "PaymentAuthorizationResult":
        return cls(
            outcome=AuthorizationOutcome(str(value.get("outcome", ""))),
            intent_id=str(value.get("intent_id", "")),
            approval_id=value.get("approval_id") if isinstance(value.get("approval_id"), str) else None,
            reserved_amount=value.get("reserved_amount") if isinstance(value.get("reserved_amount"), str) else None,
            reason=value.get("reason") if isinstance(value.get("reason"), str) else None,
        )


@dataclass(frozen=True)
class AuthorizePaymentResponse:
    authorization: PaymentAuthorizationResult
    intent: PaymentIntent
    credential: PaymentCredential | None = None
    approval: ApprovalRequest | None = None

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "AuthorizePaymentResponse":
        authorization = value.get("authorization")
        intent = value.get("intent")
        credential = value.get("credential")
        approval = value.get("approval")
        if not isinstance(authorization, Mapping) or not isinstance(intent, Mapping):
            raise ValueError("authorization response omitted authorization or intent")
        return cls(
            authorization=PaymentAuthorizationResult.from_wire(authorization),
            intent=PaymentIntent.from_wire(intent),
            credential=PaymentCredential.from_wire(credential) if isinstance(credential, Mapping) else None,
            approval=ApprovalRequest.from_wire(approval) if isinstance(approval, Mapping) else None,
        )


@dataclass(frozen=True)
class ReportOutboundResponseRequest:
    context: OperationContext
    payment_response: str

    def __post_init__(self) -> None:
        if self.context.run_id is None:
            raise ValueError("run_id is required for payment operations")
        if (
            not self.payment_response
            or len(self.payment_response) > 16 * 1024
            or not all(0x21 <= ord(character) <= 0x7E for character in self.payment_response)
        ):
            raise ValueError("payment_response must be a bounded official x402 header value")

    def wire(self) -> dict[str, Any]:
        return {**self.context.wire(), "payment_response": self.payment_response}


@dataclass(frozen=True)
class ReportOutboundResponseResponse:
    intent: PaymentIntent
    accepted: bool
    replayed: bool

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "ReportOutboundResponseResponse":
        intent = value.get("intent")
        if not isinstance(intent, Mapping):
            raise ValueError("outbound response omitted intent")
        return cls(PaymentIntent.from_wire(intent), bool(value.get("accepted")), bool(value.get("replayed")))


@dataclass(frozen=True)
class BalanceQuery:
    scenario_id: str
    wallet_id: str
    asset: AssetIdentity

    def __post_init__(self) -> None:
        required("scenario_id", self.scenario_id)
        required("wallet_id", self.wallet_id)


@dataclass(frozen=True)
class PaymentBalance:
    scenario_id: str
    wallet_id: str
    asset: AssetIdentity
    confirmed_inbound: str
    confirmed_outbound: str
    reserved_outbound: str
    available: str | None
    as_of: int

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "PaymentBalance":
        asset = value.get("asset")
        if not isinstance(asset, Mapping):
            raise ValueError("payment balance omitted asset")
        available = value.get("available")
        return cls(
            scenario_id=str(value.get("scenario_id", "")),
            wallet_id=str(value.get("wallet_id", "")),
            asset=AssetIdentity.from_wire(asset),
            confirmed_inbound=str(value.get("confirmed_inbound", "")),
            confirmed_outbound=str(value.get("confirmed_outbound", "")),
            reserved_outbound=str(value.get("reserved_outbound", "")),
            available=available if isinstance(available, str) else None,
            as_of=int(value.get("as_of", 0)),
        )


@dataclass(frozen=True)
class PayerRequestContext:
    run_id: str
    idempotency_key: str

    def __post_init__(self) -> None:
        required("run_id", self.run_id)
        OperationContext("validation", self.idempotency_key, self.run_id)

    @classmethod
    def for_operation(cls, run_id: str, operation: str) -> "PayerRequestContext":
        context = OperationContext.for_operation("validation", run_id, operation)
        return cls(run_id, context.idempotency_key)

    def operation_context(self, scenario_id: str) -> OperationContext:
        return OperationContext(scenario_id, self.idempotency_key, self.run_id)


@dataclass(frozen=True, repr=False)
class PayerHttpRequest:
    method: str
    url: str
    origin: str
    headers: Mapping[str, str] = field(default_factory=dict)
    body: bytes = b""
    follow_redirects: bool = False

    def __repr__(self) -> str:
        return (
            f"PayerHttpRequest(method={self.method!r}, url={self.url!r}, origin={self.origin!r}, "
            "headers=<redacted>, body=<redacted>, follow_redirects="
            f"{self.follow_redirects!r})"
        )


@dataclass(frozen=True)
class PayerHttpResponse:
    status: int
    headers: Mapping[str, str] = field(default_factory=dict)
    body: bytes = b""


@dataclass(frozen=True)
class AuthorizedPayment:
    intent: PaymentIntent
    credential: PaymentCredential


__all__ = [
    "AuthorizationOutcome", "AuthorizePaymentRequest", "AuthorizePaymentResponse", "AuthorizedPayment",
    "BalanceQuery", "PayerHttpRequest", "PayerHttpResponse", "PayerRequestContext", "PaymentAuthorizationResult",
    "PaymentBalance", "PaymentRequired", "ReportOutboundResponseRequest", "ReportOutboundResponseResponse",
]
