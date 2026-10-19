from __future__ import annotations

import unittest

from common.deploy.base import FullFlowEvidence
from verifier.payment.s22_payee_settlement import PAYEE_EVENTS, verify_evidence


def _evidence(facilitator_runtime: str) -> FullFlowEvidence:
    run_ids = ("run-one", "run-two")
    intent_ids = ("intent-one", "intent-two")
    payee_lines: list[str] = []
    for run_id, intent_id in zip(run_ids, intent_ids, strict=True):
        for event in PAYEE_EVENTS:
            identity = f" {intent_id}" if event in PAYEE_EVENTS[2:] else ""
            payee_lines.append(f"{event} {run_id}{identity}")
    span_ids = tuple(f"{index:016x}" for index in range(1, 7))
    payee_lines.extend(
        f"agentvault.sdk.transport.grpc.produced span_id={span_id}"
        for span_id in span_ids
    )
    vault_lines = ["agentvault.transport.grpc.request", *span_ids]
    for run_id in run_ids:
        vault_lines.extend((
            f"agentvault.facilitator.verify.completed {run_id}",
            f"agentvault.facilitator.settle.completed {run_id}",
        ))
    return FullFlowEvidence(
        payer_language="rust",
        payee_language="rust",
        transport="grpc",
        audit={
            "inbound_intent_ids": list(intent_ids),
            "run_ids": list(run_ids),
            "paid_deliveries": 2,
            "merchant_delivery_status": "delivered",
        },
        initial_balances={},
        final_balances={},
        logs={
            "payee": "\n".join(payee_lines),
            "agentvault": "\n".join(vault_lines),
            "facilitator": "",
        },
        runtime={"facilitator": facilitator_runtime},
    )


class PayeeSettlementEvidenceTest(unittest.TestCase):
    def test_accepts_capabilities_when_facilitator_stdout_is_empty(self) -> None:
        result = verify_evidence(_evidence('{"chains":["eip155:31337"],"schemes":["exact"]}'))

        self.assertIn("real Facilitator verify -> settle", result)

    def test_rejects_missing_facilitator_capabilities(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "required runtime capabilities"):
            verify_evidence(_evidence(""))


if __name__ == "__main__":
    unittest.main()
