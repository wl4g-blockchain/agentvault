from __future__ import annotations

import unittest

from common.deploy.base import FullFlowEvidence
from common.payer_identity import PAYER_IDENTITIES, payer_identity
from verifier.payment.s24_gateway_balance import UNIT_PRICE, verify_evidence


def _evidence() -> FullFlowEvidence:
    identity = payer_identity("rust")
    initial = {
        wallet_id: 10 * UNIT_PRICE
        for candidate in PAYER_IDENTITIES.values()
        for wallet_id in candidate.wallet_ids
    }
    initial["payee"] = 0
    final = dict(initial)
    for wallet_id in identity.wallet_ids:
        final[wallet_id] -= UNIT_PRICE
    final["payee"] += 2 * UNIT_PRICE
    transaction_hashes = ("0x" + "11" * 32, "0x" + "22" * 32)
    ledger_rows = []
    for index, transaction_hash in enumerate(transaction_hashes, start=1):
        for direction in ("outbound", "inbound"):
            ledger_rows.append({
                "run_id": f"run-{index}",
                "intent_id": f"intent-{index}-{direction}",
                "direction": direction,
                "tx_hash": transaction_hash,
            })
    receipts = [
        {"transaction_hash": transaction_hash, "gas_fee_wei": index * 21_000}
        for index, transaction_hash in enumerate(transaction_hashes, start=1)
    ]
    gas_fees = sum(int(receipt["gas_fee_wei"]) for receipt in receipts)
    return FullFlowEvidence(
        payer_language="rust",
        payee_language="rust",
        transport="grpc",
        audit={},
        initial_balances=initial,
        final_balances=final,
        chain_accounting={
            "network": "eip155:31337",
            "facilitator_native_spent": gas_fees,
            "receipt_gas_fees": gas_fees,
            "ledger_rows": ledger_rows,
            "receipts": receipts,
        },
        logs={},
        runtime={},
    )


class GatewayBalanceEvidenceTest(unittest.TestCase):
    def test_accepts_exact_token_ledger_receipt_and_gas_conservation(self) -> None:
        result = verify_evidence(_evidence())

        self.assertIn("native Gas conservation", result)

    def test_rejects_duplicate_transaction_accounting(self) -> None:
        evidence = _evidence()
        duplicate = evidence.chain_accounting["receipts"][0]["transaction_hash"]
        evidence.chain_accounting["receipts"][1]["transaction_hash"] = duplicate

        with self.assertRaisesRegex(RuntimeError, "exactly bound"):
            verify_evidence(evidence)

    def test_rejects_unaccounted_facilitator_native_spend(self) -> None:
        evidence = _evidence()
        evidence.chain_accounting["facilitator_native_spent"] += 1

        with self.assertRaisesRegex(RuntimeError, "do not conserve"):
            verify_evidence(evidence)


if __name__ == "__main__":
    unittest.main()
