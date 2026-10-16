from __future__ import annotations

import urllib.parse

from .config import PayeeConfig, PayerConfig
from .model import ApprovalPage, ApprovalQuery, ApprovalRequest, SubmitApprovalDecisionRequest
from .payee import PayeeMiddleware
from .payer import PayerMiddleware
from .transport import VaultTransport


class AgentVault:
    """Single SDK facade that creates role middleware from one transport."""

    def __init__(self, transport: VaultTransport) -> None:
        if transport is None:
            raise ValueError("AgentVault transport is not configured")
        self._transport = transport

    def payer(self, config: PayerConfig) -> PayerMiddleware:
        return PayerMiddleware(self._transport, config)

    def payee(self, config: PayeeConfig | None = None) -> PayeeMiddleware:
        return PayeeMiddleware(self._transport, config or PayeeConfig())

    def get_pending_approvals(self, query: ApprovalQuery) -> ApprovalPage:
        encoded = urllib.parse.urlencode({"scenario_id": query.scenario_id, "status": "pending"})
        return ApprovalPage.from_wire(self._transport.request("GET", f"/api/v1/approvals?{encoded}"))

    def submit_approval_decision(self, request: SubmitApprovalDecisionRequest) -> ApprovalRequest:
        approval = urllib.parse.quote(request.approval_id, safe="")
        value = self._transport.request("POST", f"/api/v1/approvals/{approval}/decision",
                                        request.wire(), request.context.idempotency_key)
        return ApprovalRequest.from_wire(value)

    def get_approval_status(self, scenario_id: str, approval_id: str) -> ApprovalRequest:
        if not scenario_id.strip() or not approval_id.strip():
            raise ValueError("scenario_id and approval_id cannot be blank")
        query = urllib.parse.urlencode({"scenario_id": scenario_id})
        approval = urllib.parse.quote(approval_id, safe="")
        value = self._transport.request("GET", f"/api/v1/approvals/{approval}?{query}")
        return ApprovalRequest.from_wire(value)
