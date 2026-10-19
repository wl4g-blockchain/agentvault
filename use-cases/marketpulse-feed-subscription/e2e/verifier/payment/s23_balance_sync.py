from __future__ import annotations

from common.deploy.base import FullFlowEvidence


def verify_evidence(evidence: FullFlowEvidence, database_evidence: str) -> str:
    vault_logs = evidence.logs.get("agentvault", "")
    for event in ("agentvault.wallet.balance_synced", "agentvault.wallet.balance_sync_completed"):
        if event not in vault_logs:
            raise RuntimeError(f"periodic balance synchronization omitted {event}")
    if "periodic SQL balance snapshots converged" not in database_evidence:
        raise RuntimeError("periodic balance snapshots did not converge to chain state")
    return (
        "verified AgentVault-owned periodic chain observation -> SQL snapshot flow; "
        "Payer application evidence contains no chain operation; " + database_evidence
    )
