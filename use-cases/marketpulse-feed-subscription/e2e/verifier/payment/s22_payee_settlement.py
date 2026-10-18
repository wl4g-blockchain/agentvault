from __future__ import annotations

import re

from common.deploy.base import FullFlowEvidence


PAYEE_EVENTS = (
    "agentvault.sdk.payee.intercepted",
    "agentvault.sdk.payee.challenge_created",
    "agentvault.sdk.payee.verified",
    "agentvault.sdk.payee.settled",
    "agentvault.sdk.payee.resource_released",
)


def _event_lines(logs: str, event: str) -> list[str]:
    return [line for line in logs.splitlines() if event in line]


def _position(logs: str, event: str, identity: str) -> int:
    for position, line in enumerate(logs.splitlines()):
        if event in line and identity in line:
            return position
    raise RuntimeError(f"{event} is not bound to {identity}")


def _require_bound(logs: str, event: str, *identities: str) -> None:
    if not any(all(identity in line for identity in identities) for line in _event_lines(logs, event)):
        raise RuntimeError(f"{event} is not bound to {', '.join(identities)}")


def verify_evidence(evidence: FullFlowEvidence) -> str:
    payee_logs = evidence.logs.get("payee", "")
    vault_logs = evidence.logs.get("agentvault", "")
    facilitator_logs = evidence.logs.get("facilitator", "")
    missing = [event for event in PAYEE_EVENTS if event not in payee_logs]
    if missing:
        raise RuntimeError("payee evidence omitted events: " + ", ".join(missing))
    producer_event = f"agentvault.sdk.transport.{evidence.transport}.produced"
    consumer_event = f"agentvault.transport.{evidence.transport}.request"
    span_ids = {
        match.group(1)
        for line in _event_lines(payee_logs, producer_event)
        if (match := re.search(r'(?:span[._]?id|spanId)["\s:=]+([0-9a-f]{16})', line))
    }
    if len(span_ids) < 6 or any(span_id not in vault_logs for span_id in span_ids):
        raise RuntimeError(
            f"Payee {evidence.transport} producer spans are not propagated one-to-one into AgentVault consumer logs"
        )
    if consumer_event not in vault_logs:
        raise RuntimeError(f"payee evidence omitted {consumer_event}")
    inbound = evidence.audit.get("inbound_intent_ids", [])
    if not isinstance(inbound, list) or len(inbound) != 2:
        raise RuntimeError("payee evidence must contain two settled inbound intents")
    run_ids = evidence.audit.get("run_ids", [])
    if not isinstance(run_ids, list) or len(run_ids) != 2 or len(set(run_ids)) != 2:
        raise RuntimeError("payee evidence must contain two distinct workflow run IDs")
    for run_id, intent_id in zip(run_ids, inbound, strict=True):
        positions = [_position(payee_logs, event, run_id) for event in PAYEE_EVENTS]
        if positions != sorted(positions):
            raise RuntimeError(f"payee middleware events are out of order for run {run_id}: {positions}")
        for event in PAYEE_EVENTS[2:]:
            _require_bound(payee_logs, event, run_id, intent_id)
        _require_bound(vault_logs, "agentvault.facilitator.verify.completed", run_id)
        _require_bound(vault_logs, "agentvault.facilitator.settle.completed", run_id)
    if evidence.audit.get("paid_deliveries") != 2 or evidence.audit.get("merchant_delivery_status") != "delivered":
        raise RuntimeError("MarketPulse released data without two durable settled deliveries")
    if not facilitator_logs.strip():
        raise RuntimeError("real Facilitator produced no runtime evidence")
    return (
        f"verified {evidence.payee_language} PayeeMiddleware: each run is bound across intercept -> challenge -> "
        f"real Facilitator verify -> settle -> resource release; runs={','.join(run_ids)} "
        f"inbound_intents={','.join(inbound)}"
    )
