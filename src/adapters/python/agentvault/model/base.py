from __future__ import annotations

import re
from dataclasses import dataclass
from enum import Enum
from typing import Any, Mapping

_IDENTIFIER = re.compile(r"^[A-Za-z0-9_.:-]{1,128}$")


def required(name: str, value: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise ValueError(f"{name} cannot be blank")
    return value


def _mapping(name: str, value: Any) -> Mapping[str, Any]:
    if not isinstance(value, Mapping):
        raise ValueError(f"{name} must be an object")
    return value


def _text(value: Any, default: str = "") -> str:
    return value if isinstance(value, str) else default


def _optional_text(value: Any) -> str | None:
    return value if isinstance(value, str) else None


def _optional_int(value: Any) -> int | None:
    return value if isinstance(value, int) and not isinstance(value, bool) else None


class PaymentDirection(str, Enum):
    INBOUND = "inbound"
    OUTBOUND = "outbound"


class PaymentStatus(str, Enum):
    CREATED = "created"
    PENDING_APPROVAL = "pending_approval"
    AUTHORIZED = "authorized"
    SIGNED = "signed"
    SUBMITTED = "submitted"
    PENDING_SETTLEMENT = "pending_settlement"
    SETTLED = "settled"
    FAILED = "failed"
    EXPIRED = "expired"
    UNKNOWN = "unknown"


class ReservationStatus(str, Enum):
    UNRESERVED = "unreserved"
    RESERVED = "reserved"
    CONSUMED = "consumed"
    RELEASED = "released"


@dataclass(frozen=True)
class OperationContext:
    scenario_id: str
    run_id: str

    def __post_init__(self) -> None:
        required("scenario_id", self.scenario_id)
        required("run_id", self.run_id)
        if not _IDENTIFIER.fullmatch(self.run_id):
            raise ValueError("run_id contains unsupported characters or length")

    def wire(self) -> dict[str, Any]:
        return {"scenario_id": self.scenario_id, "run_id": self.run_id}


@dataclass(frozen=True)
class AssetIdentity:
    network: str
    asset: str
    decimals: int
    token_contract: str | None = None

    def __post_init__(self) -> None:
        required("network", self.network)
        required("asset", self.asset)
        if not 0 <= self.decimals <= 255:
            raise ValueError("decimals must be in 0..255")

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "AssetIdentity":
        return cls(
            network=_text(value.get("network")),
            asset=_text(value.get("asset")),
            decimals=value.get("decimals") if isinstance(value.get("decimals"), int) else -1,
            token_contract=_optional_text(value.get("token_contract")),
        )

    def wire(self) -> dict[str, Any]:
        value: dict[str, Any] = {"network": self.network, "asset": self.asset, "decimals": self.decimals}
        if self.token_contract is not None:
            value["token_contract"] = self.token_contract
        return value


@dataclass(frozen=True, repr=False)
class HttpCredential:
    bound_origin: str
    header_name: str
    header_value: str

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "HttpCredential":
        return cls(
            bound_origin=_text(value.get("bound_origin")),
            header_name=_text(value.get("header_name")),
            header_value=_text(value.get("header_value")),
        )

    def wire(self) -> dict[str, str]:
        return {
            "bound_origin": self.bound_origin,
            "header_name": self.header_name,
            "header_value": self.header_value,
        }

    def __repr__(self) -> str:
        return f"HttpCredential(bound_origin={self.bound_origin!r}, header_name={self.header_name!r}, value=<redacted>)"


@dataclass(frozen=True, repr=False)
class PaymentCredential:
    protocol: str
    challenge_hash: str
    credential_hash: str
    payload: Any
    http: HttpCredential | None = None

    def __post_init__(self) -> None:
        required("credential.protocol", self.protocol)
        required("credential.challenge_hash", self.challenge_hash)
        required("credential.credential_hash", self.credential_hash)

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "PaymentCredential":
        http = value.get("http")
        return cls(
            protocol=_text(value.get("protocol")),
            challenge_hash=_text(value.get("challenge_hash")),
            credential_hash=_text(value.get("credential_hash")),
            payload=value.get("payload"),
            http=HttpCredential.from_wire(_mapping("credential.http", http)) if http is not None else None,
        )

    def wire(self) -> dict[str, Any]:
        value: dict[str, Any] = {
            "protocol": self.protocol,
            "challenge_hash": self.challenge_hash,
            "credential_hash": self.credential_hash,
            "payload": self.payload,
        }
        if self.http is not None:
            value["http"] = self.http.wire()
        return value

    def __repr__(self) -> str:
        return f"PaymentCredential(protocol={self.protocol!r}, credential=<redacted>)"


@dataclass(frozen=True)
class PaymentIntent:
    id: str
    scenario_id: str
    run_id: str
    wallet_id: str | None
    policy_id: str | None
    price_id: str | None
    direction: PaymentDirection
    protocol: str
    asset: AssetIdentity
    recipient: str
    amount: str
    settled_amount: str
    resource: str
    challenge_hash: str | None
    request_hash: str
    status: PaymentStatus
    reservation_status: ReservationStatus
    reserved_amount: str
    tx_hash: str | None
    settlement_ref: str | None
    next_reconcile_at: int | None
    created_at: int
    updated_at: int
    expires_at: int | None
    settled_at: int | None

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "PaymentIntent":
        return cls(
            id=_text(value.get("id")),
            scenario_id=_text(value.get("scenario_id")),
            run_id=_text(value.get("run_id")),
            wallet_id=_optional_text(value.get("wallet_id")),
            policy_id=_optional_text(value.get("policy_id")),
            price_id=_optional_text(value.get("price_id")),
            direction=PaymentDirection(_text(value.get("direction"))),
            protocol=_text(value.get("protocol")),
            asset=AssetIdentity.from_wire(_mapping("intent.asset", value.get("asset"))),
            recipient=_text(value.get("recipient")),
            amount=_text(value.get("amount")),
            settled_amount=_text(value.get("settled_amount")),
            resource=_text(value.get("resource")),
            challenge_hash=_optional_text(value.get("challenge_hash")),
            request_hash=_text(value.get("request_hash")),
            status=PaymentStatus(_text(value.get("status"))),
            reservation_status=ReservationStatus(_text(value.get("reservation_status"))),
            reserved_amount=_text(value.get("reserved_amount")),
            tx_hash=_optional_text(value.get("tx_hash")),
            settlement_ref=_optional_text(value.get("settlement_ref")),
            next_reconcile_at=_optional_int(value.get("next_reconcile_at")),
            created_at=int(value.get("created_at", 0)),
            updated_at=int(value.get("updated_at", 0)),
            expires_at=_optional_int(value.get("expires_at")),
            settled_at=_optional_int(value.get("settled_at")),
        )


@dataclass(frozen=True)
class PaymentChallenge:
    scenario_id: str
    price_id: str | None
    resource: str
    protocol: str
    asset: AssetIdentity
    amount: str
    recipient: str
    nonce: str
    challenge_hash: str
    expires_at: int
    payload: Any

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "PaymentChallenge":
        return cls(
            scenario_id=_text(value.get("scenario_id")),
            price_id=_optional_text(value.get("price_id")),
            resource=_text(value.get("resource")),
            protocol=_text(value.get("protocol")),
            asset=AssetIdentity.from_wire(_mapping("challenge.asset", value.get("asset"))),
            amount=_text(value.get("amount")),
            recipient=_text(value.get("recipient")),
            nonce=_text(value.get("nonce")),
            challenge_hash=_text(value.get("challenge_hash")),
            expires_at=int(value.get("expires_at", 0)),
            payload=value.get("payload"),
        )


@dataclass(frozen=True)
class PaymentReceipt:
    intent_id: str
    scenario_id: str
    direction: PaymentDirection
    protocol: str
    asset: AssetIdentity
    recipient: str
    payer: str | None
    requested_amount: str
    settled_amount: str
    status: PaymentStatus
    tx_hash: str | None
    settlement_ref: str | None
    evidence_hash: str
    confirmed_at: int

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "PaymentReceipt":
        return cls(
            intent_id=_text(value.get("intent_id")),
            scenario_id=_text(value.get("scenario_id")),
            direction=PaymentDirection(_text(value.get("direction"))),
            protocol=_text(value.get("protocol")),
            asset=AssetIdentity.from_wire(_mapping("receipt.asset", value.get("asset"))),
            recipient=_text(value.get("recipient")),
            payer=_optional_text(value.get("payer")),
            requested_amount=_text(value.get("requested_amount")),
            settled_amount=_text(value.get("settled_amount")),
            status=PaymentStatus(_text(value.get("status"))),
            tx_hash=_optional_text(value.get("tx_hash")),
            settlement_ref=_optional_text(value.get("settlement_ref")),
            evidence_hash=_text(value.get("evidence_hash")),
            confirmed_at=int(value.get("confirmed_at", 0)),
        )
