from __future__ import annotations

import json
import unittest
from unittest.mock import Mock, patch

from verifier.payment.s26_x402_solana import verify


class X402SolanaVerifierTest(unittest.TestCase):
    @staticmethod
    def evidence(**overrides: object) -> str:
        values: dict[str, object] = {
            "version": 2,
            "scheme": "exact",
            "network": "solana:" + "A" * 32,
            "transaction": "B" * 88,
            "payer": "C" * 44,
            "recipient": "D" * 44,
            "mint": "E" * 44,
            "amount": 100,
            "payer_token_before": 1000,
            "payer_token_after": 900,
            "recipient_token_before": 10,
            "recipient_token_after": 110,
            "payer_native_before": 0,
            "payer_native_after": 0,
            "facilitator_native_before": 10000,
            "facilitator_native_after": 9993,
            "transaction_fee": 7,
            "verify_before_settle": True,
            "broadcast_count": 1,
            "vault_protocol_adapter": True,
            "vault_custody_signing": True,
            "vault_handler_roundtrip": True,
            "outbound_receipt_reconciled": True,
            "payer_ledger_entries": 1,
            "payee_ledger_entries": 1,
            "settlement_requested_events": 1,
        }
        values.update(overrides)
        return "X402_SOLANA_EVIDENCE=" + json.dumps(values, separators=(",", ":"))

    @patch("verifier.payment.s26_x402_solana.create_deployer")
    def test_verifies_exact_token_and_lamport_conservation(self, create_deployer: Mock) -> None:
        create_deployer.return_value.x402_solana.return_value = self.evidence()

        report = verify(Mock())

        self.assertIn("official facilitator", report)
        self.assertIn("broadcasts=1", report)
        self.assertIn("token_conserved=1010", report)

    @patch("verifier.payment.s26_x402_solana.create_deployer")
    def test_rejects_duplicate_broadcast(self, create_deployer: Mock) -> None:
        create_deployer.return_value.x402_solana.return_value = self.evidence(broadcast_count=2)

        with self.assertRaisesRegex(RuntimeError, "protocol evidence"):
            verify(Mock())

    @patch("verifier.payment.s26_x402_solana.create_deployer")
    def test_rejects_unconserved_facilitator_fee(self, create_deployer: Mock) -> None:
        create_deployer.return_value.x402_solana.return_value = self.evidence(facilitator_native_after=9994)

        with self.assertRaisesRegex(RuntimeError, "lamport delta"):
            verify(Mock())


if __name__ == "__main__":
    unittest.main()
