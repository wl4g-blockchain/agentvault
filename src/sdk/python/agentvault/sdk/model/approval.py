from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
from typing import Any, Mapping

from .base import OperationContext, required


class ApprovalStatus(str, Enum):
    PENDING = "pending"
    APPROVED = "approved"
    REJECTED = "rejected"
    EXPIRED = "expired"
    CONSUMED = "consumed"


class ApprovalDecisionKind(str, Enum):
    APPROVE = "approve"
    REJECT = "reject"


@dataclass(frozen=True)
class ApprovalRequest:
    id: str
    intent_id: str
    run_id: str
    requested_amount: str
    approved_amount: str | None
    requested_by: str
    approved_by: str | None
    status: ApprovalStatus
    intent_hash: str
    grant_jti: str | None
    grant_claims_hash: str | None
    expires_at: int
    created_at: int
    approved_at: int | None
    consumed_at: int | None

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "ApprovalRequest":
        text = lambda name: str(value.get(name, ""))
        optional_text = lambda name: value.get(name) if isinstance(value.get(name), str) else None
        optional_int = lambda name: value.get(name) if isinstance(value.get(name), int) else None
        return cls(
            id=text("id"),
            intent_id=text("intent_id"),
            run_id=text("run_id"),
            requested_amount=text("requested_amount"),
            approved_amount=optional_text("approved_amount"),
            requested_by=text("requested_by"),
            approved_by=optional_text("approved_by"),
            status=ApprovalStatus(text("status")),
            intent_hash=text("intent_hash"),
            grant_jti=optional_text("grant_jti"),
            grant_claims_hash=optional_text("grant_claims_hash"),
            expires_at=int(value.get("expires_at", 0)),
            created_at=int(value.get("created_at", 0)),
            approved_at=optional_int("approved_at"),
            consumed_at=optional_int("consumed_at"),
        )


@dataclass(frozen=True)
class ApprovalQuery:
    scenario_id: str

    def __post_init__(self) -> None:
        required("scenario_id", self.scenario_id)


@dataclass(frozen=True)
class ApprovalPage:
    items: tuple[ApprovalRequest, ...]
    total: int

    @classmethod
    def from_wire(cls, value: Mapping[str, Any]) -> "ApprovalPage":
        items = value.get("items")
        if not isinstance(items, list):
            raise ValueError("approval page items must be an array")
        return cls(tuple(ApprovalRequest.from_wire(item) for item in items), int(value.get("total", 0)))


@dataclass(frozen=True)
class SubmitApprovalDecisionRequest:
    context: OperationContext
    approval_id: str
    decision: ApprovalDecisionKind
    approved_amount: str | None = None
    reason: str | None = None

    def __post_init__(self) -> None:
        required("approval_id", self.approval_id)
        if self.decision is ApprovalDecisionKind.APPROVE:
            required("approved_amount", self.approved_amount or "")
        elif self.approved_amount is not None:
            raise ValueError("approved_amount must be omitted when rejecting")

    def wire(self) -> dict[str, object]:
        value: dict[str, object] = {
            **self.context.wire(), "approval_id": self.approval_id, "decision": self.decision.value,
        }
        if self.approved_amount is not None:
            value["approved_amount"] = self.approved_amount
        if self.reason is not None:
            value["reason"] = self.reason
        return value
