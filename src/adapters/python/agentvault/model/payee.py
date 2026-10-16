from __future__ import annotations

from dataclasses import dataclass
from typing import Any, Mapping

from .base import (
    AssetIdentity,
    OperationContext,
    PaymentChallenge,
    PaymentCredential,
    PaymentIntent,
    PaymentReceipt,
    required,
)


@dataclass(frozen=True)
class ResolveOfferRequest:
    scenario_id: str
    resource: str
    protocol: str | None = None

    def __post_init__(self) -> None:
        required("scenario_id", self.scenario_id)
        required("resource", self.resource)
        if self.protocol is not None:
            required("protocol", self.protocol)

    def wire(self) -> dict[str, str]:
        value = {"scenario_id": self.scenario_id, "resource": self.resource}
        if self.protocol is not None:
            value["protocol"] = self.protocol
        return value


@dataclass(frozen=True)
class PaymentOffer:
    id: str
    scenario_id: str
    resource: str
    protocols: tuple[str, ...]
    asset: AssetIdentity
    amount: str
    wallet_id: str
    enabled: bool
    created_at: int
    updated_at: int

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "PaymentOffer":
        asset = value.get("asset")
        protocols = value.get("protocols")
        if not isinstance(asset, Mapping) or not isinstance(protocols, list):
            raise ValueError("payment offer omitted asset or protocols")
        return cls(
            id=str(value.get("id", "")),
            scenario_id=str(value.get("scenario_id", "")),
            resource=str(value.get("resource", "")),
            protocols=tuple(str(protocol) for protocol in protocols),
            asset=AssetIdentity.from_wire(asset),
            amount=str(value.get("amount", "")),
            wallet_id=str(value.get("wallet_id", "")),
            enabled=bool(value.get("enabled")),
            created_at=int(value.get("created_at", 0)),
            updated_at=int(value.get("updated_at", 0)),
        )


@dataclass(frozen=True)
class PayeeOperation:
    scenario_id: str
    run_id: str
    execution_key: str
    offer_id: str
    resource: str
    origin: str
    protocol: str
    request_hash: str
    credential_payload: Any | None = None

    def __post_init__(self) -> None:
        for name, value in (
            ("scenario_id", self.scenario_id), ("run_id", self.run_id),
            ("execution_key", self.execution_key), ("offer_id", self.offer_id),
            ("resource", self.resource), ("origin", self.origin),
            ("protocol", self.protocol), ("request_hash", self.request_hash),
        ):
            required(name, value)
        if len(self.execution_key) > 96 or any(
            not (character.isascii() and (character.isalnum() or character in "-_."))
            for character in self.execution_key
        ):
            raise ValueError("execution_key must contain 1..=96 safe ASCII characters")

    def challenge_context(self) -> OperationContext:
        return OperationContext(self.scenario_id, f"{self.execution_key}.challenge", self.run_id)

    def settlement_context(self) -> OperationContext:
        return OperationContext(self.scenario_id, f"{self.execution_key}.settle", self.run_id)


@dataclass(frozen=True)
class CreateChallengeRequest:
    context: OperationContext
    offer_id: str
    resource: str
    origin: str
    protocol: str
    request_hash: str

    def wire(self) -> dict[str, Any]:
        return {
            **self.context.wire(), "offer_id": self.offer_id, "resource": self.resource,
            "origin": self.origin, "protocol": self.protocol, "request_hash": self.request_hash,
        }


@dataclass(frozen=True)
class VerifyPaymentRequest:
    context: OperationContext
    challenge_hash: str
    request_hash: str
    credential: PaymentCredential

    def wire(self) -> dict[str, Any]:
        return {
            **self.context.wire(), "challenge_hash": self.challenge_hash,
            "request_hash": self.request_hash, "credential": self.credential.wire(),
        }


@dataclass(frozen=True)
class PaymentVerification:
    intent: PaymentIntent
    valid: bool
    verification_ref: str | None = None

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "PaymentVerification":
        intent = value.get("intent")
        if not isinstance(intent, Mapping):
            raise ValueError("payment verification omitted intent")
        reference = value.get("verification_ref")
        return cls(
            intent=PaymentIntent.from_wire(intent),
            valid=bool(value.get("valid")),
            verification_ref=reference if isinstance(reference, str) else None,
        )


@dataclass(frozen=True)
class SettlePaymentRequest:
    context: OperationContext
    execution_key: str
    intent_id: str
    verification_ref: str

    def wire(self) -> dict[str, Any]:
        return {
            **self.context.wire(), "execution_key": self.execution_key,
            "intent_id": self.intent_id, "verification_ref": self.verification_ref,
        }


@dataclass(frozen=True)
class IncomingPayment:
    scenario_id: str
    run_id: str
    offer_id: str
    resource: str
    idempotency_key: str
    request_hash: str
    credential: PaymentCredential


@dataclass(frozen=True)
class SettledIncomingPayment:
    intent: PaymentIntent
    receipt: PaymentReceipt
    execution_key: str

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "SettledIncomingPayment":
        intent = value.get("intent")
        receipt = value.get("receipt")
        if not isinstance(intent, Mapping) or not isinstance(receipt, Mapping):
            raise ValueError("settled payment omitted intent or receipt")
        return cls(
            intent=PaymentIntent.from_wire(intent),
            receipt=PaymentReceipt.from_wire(receipt),
            execution_key=str(value.get("execution_key", "")),
        )


@dataclass(frozen=True)
class HttpProtectionRequest:
    context: OperationContext
    resource: str
    origin: str
    credential: PaymentCredential | None
    request_hash: str


@dataclass(frozen=True)
class McpProtectionRequest:
    context: OperationContext
    tool: str
    arguments_hash: str
    credential: PaymentCredential | None


@dataclass(frozen=True)
class ProtectionOutcome:
    outcome: str
    challenge: PaymentChallenge | None = None
    payment: SettledIncomingPayment | None = None

    @classmethod
    def payment_required(cls, challenge: PaymentChallenge) -> "ProtectionOutcome":
        return cls("payment_required", challenge=challenge)

    @classmethod
    def authorized(cls, payment: SettledIncomingPayment) -> "ProtectionOutcome":
        return cls("authorized", payment=payment)


__all__ = [
    "CreateChallengeRequest", "HttpProtectionRequest", "IncomingPayment", "McpProtectionRequest",
    "PayeeOperation", "PaymentOffer", "PaymentVerification", "ProtectionOutcome", "ResolveOfferRequest",
    "SettlePaymentRequest", "SettledIncomingPayment",
]
