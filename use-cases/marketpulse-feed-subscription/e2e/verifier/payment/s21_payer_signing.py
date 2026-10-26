from __future__ import annotations

import re

from deploy.deployer.base import FullFlowEvidence
from common.payer_identity import PAYER_IDENTITIES, payer_identity


PAYER_EVENTS = (
    "agentvault.sdk.payer.payment_required",
    "agentvault.sdk.payer.signing_requested",
    "agentvault.sdk.payer.signed",
    "agentvault.sdk.payer.retrying",
)


def _event_lines(logs: str, event: str) -> list[str]:
    return [line for line in logs.splitlines() if event in line]


def _bound_line(logs: str, event: str, *identities: str) -> str:
    matches = [line for line in _event_lines(logs, event) if all(value in line for value in identities)]
    if not matches:
        raise RuntimeError(f"{event} is not bound to {', '.join(identities)}")
    return matches[0]


def _position(logs: str, event: str, identity: str) -> int:
    for position, line in enumerate(logs.splitlines()):
        if event in line and identity in line:
            return position
    raise RuntimeError(f"{event} is not bound to {identity}")


def verify_evidence(evidence: FullFlowEvidence) -> str:
    identity = payer_identity(evidence.payer_language)
    payer_logs = evidence.logs.get("payer", "")
    vault_logs = evidence.logs.get("agentvault", "")
    missing = [
        event for event in ("agentvault.sdk.payer.transport_selected", *PAYER_EVENTS)
        if event not in payer_logs
    ]
    transport_event = f"agentvault.transport.{evidence.transport}.request"
    for event in (transport_event, "agentvault.wallet.selected", "agentvault.wallet.signed"):
        if event not in vault_logs:
            missing.append(event)
    if missing:
        raise RuntimeError("payer/signing evidence omitted events: " + ", ".join(missing))
    outbound = evidence.audit.get("outbound_intent_ids", [])
    if not isinstance(outbound, list) or len(outbound) != 2:
        raise RuntimeError("payer evidence must contain two settled outbound intents")
    run_ids = evidence.audit.get("run_ids", [])
    if not isinstance(run_ids, list) or len(run_ids) != 2 or len(set(run_ids)) != 2:
        raise RuntimeError("payer evidence must contain two distinct workflow run IDs")
    if len(_event_lines(vault_logs, transport_event)) < 4:
        raise RuntimeError(f"{transport_event} did not carry authorize and report calls for both runs")
    producer_event = f"agentvault.sdk.transport.{evidence.transport}.produced"
    produced = _event_lines(payer_logs, producer_event)
    span_ids = {
        match.group(1)
        for line in produced
        if (match := re.search(r'(?:span[._]?id|spanId)["\s:=]+([0-9a-f]{16})', line))
    }
    if len(span_ids) < 4 or any(span_id not in vault_logs for span_id in span_ids):
        raise RuntimeError(
            f"{evidence.transport} producer spans are not propagated one-to-one into AgentVault consumer logs"
        )
    for run_id, intent_id in zip(run_ids, outbound, strict=True):
        positions = [_position(payer_logs, event, run_id) for event in PAYER_EVENTS]
        if positions != sorted(positions):
            raise RuntimeError(f"payer middleware events are out of order for run {run_id}: {positions}")
        _bound_line(payer_logs, "agentvault.sdk.payer.signed", run_id, intent_id)
        _bound_line(payer_logs, "agentvault.sdk.payer.retrying", run_id, intent_id)
        selected = _bound_line(vault_logs, "agentvault.wallet.selected", run_id)
        signed = _bound_line(vault_logs, "agentvault.wallet.signed", run_id, intent_id)
        if vault_logs.index(selected) > vault_logs.index(signed):
            raise RuntimeError(f"wallet signing preceded scenario selection for run {run_id}")
    for wallet in identity.wallet_ids:
        if not any(wallet in line for event in ("agentvault.wallet.selected", "agentvault.wallet.signed")
                   for line in _event_lines(vault_logs, event)):
            raise RuntimeError(f"wallet-pool signing evidence omitted {wallet}")
    foreign_wallets = {
        wallet_id
        for candidate in PAYER_IDENTITIES.values()
        if candidate.language != identity.language
        for wallet_id in candidate.wallet_ids
    }
    leaked = sorted(
        wallet_id
        for wallet_id in foreign_wallets
        if any(wallet_id in line for event in ("agentvault.wallet.selected", "agentvault.wallet.signed")
               for line in _event_lines(vault_logs, event))
    )
    if leaked:
        raise RuntimeError(f"payer signing crossed language wallet pools: {leaked}")
    return (
        f"verified {evidence.payer_language} PayerMiddleware over {evidence.transport}: "
        "each run is bound across 402 interception -> signing request -> scenario wallet-pool selection -> "
        "EOA signature -> paid retry -> response report; "
        f"runs={','.join(run_ids)} outbound_intents={','.join(outbound)} "
        f"wallets={','.join(identity.wallet_ids)}"
    )
