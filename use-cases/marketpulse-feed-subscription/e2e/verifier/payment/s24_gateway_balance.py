from __future__ import annotations

from deploy.deployer.base import FullFlowEvidence
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
    accounting = evidence.chain_accounting
    ledger_rows = accounting.get("ledger_rows")
    receipts = accounting.get("receipts")
    if accounting.get("network") != "eip155:31337" or accounting.get("facilitator_native_spent") != accounting.get(
        "receipt_gas_fees"
    ):
        raise RuntimeError("facilitator native balance and receipt gas accounting do not conserve")
    if not isinstance(ledger_rows, list) or len(ledger_rows) != 4:
        raise RuntimeError("chain accounting must contain four exact bidirectional payment-ledger rows")
    if not isinstance(receipts, list) or len(receipts) != 2:
        raise RuntimeError("chain accounting must contain exactly two settlement receipts")
    transaction_hashes = {str(receipt.get("transaction_hash", "")) for receipt in receipts}
    if len(transaction_hashes) != 2 or {str(row.get("tx_hash", "")) for row in ledger_rows} != transaction_hashes:
        raise RuntimeError("payment-ledger transaction hashes are not exactly bound to settlement receipts")
    if any(int(receipt.get("gas_fee_wei", 0)) <= 0 for receipt in receipts):
        raise RuntimeError("a settlement receipt has no positive gas fee")
    run_pairs: dict[str, list[dict[str, object]]] = {}
    for row in ledger_rows:
        run_pairs.setdefault(str(row.get("run_id", "")), []).append(row)
    if len(run_pairs) != 2 or any(
        len(rows) != 2
        or {row.get("direction") for row in rows} != {"outbound", "inbound"}
        or len({row.get("tx_hash") for row in rows}) != 1
        for rows in run_pairs.values()
    ):
        raise RuntimeError("each workflow run must have one outbound/inbound ledger pair for one transaction")
    return (
        "verified exact post-settlement ERC-20 balances, bidirectional ledger/receipt binding, and native Gas "
        "conservation across ten isolated Payer wallets: "
        f"active_wallets={identity.wallet_ids}, inactive_wallets_unchanged=8, "
        f"initial={initial}, final={final}, unit_price={UNIT_PRICE}, "
        f"transactions={sorted(transaction_hashes)}, gas_wei={accounting['receipt_gas_fees']}"
    )
