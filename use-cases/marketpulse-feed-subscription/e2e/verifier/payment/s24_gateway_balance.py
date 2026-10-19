from __future__ import annotations

from common.deploy.base import FullFlowEvidence
from common.payer_identity import PAYER_IDENTITIES, payer_identity


UNIT_PRICE = 1_000_000


def verify_evidence(evidence: FullFlowEvidence) -> str:
    initial = evidence.initial_balances
    final = evidence.final_balances
    identity = payer_identity(evidence.payer_language)
    expected_keys = {
        "payee",
        *(wallet_id for candidate in PAYER_IDENTITIES.values() for wallet_id in candidate.wallet_ids),
    }
    if set(initial) != expected_keys or set(final) != expected_keys:
        raise RuntimeError(
            f"balance evidence must cover ten isolated Payer wallets plus Payee: "
            f"initial={sorted(initial)} final={sorted(final)}"
        )
    expected = dict(initial)
    for wallet_id in identity.wallet_ids:
        expected[wallet_id] -= UNIT_PRICE
    expected["payee"] += 2 * UNIT_PRICE
    if final != expected:
        raise RuntimeError(f"settlement balance mismatch: expected={expected}, actual={final}")
    if sum(initial.values()) != sum(final.values()):
        raise RuntimeError("gateway token balance was not conserved across settlement")
    return (
        "verified exact post-settlement ERC-20 balances and conservation across ten isolated Payer wallets: "
        f"active_wallets={identity.wallet_ids}, inactive_wallets_unchanged=8, "
        f"initial={initial}, final={final}, unit_price={UNIT_PRICE}"
    )
