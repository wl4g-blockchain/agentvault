from __future__ import annotations

import unittest
from unittest.mock import Mock, patch

from verifier.payment.s25_mpp_tempo import verify


class MppTempoVerifierTest(unittest.TestCase):
    @staticmethod
    def evidence(*, broadcasts: int = 1, payer_ledger_entries: int = 1) -> str:
        return (
            'MPP_TEMPO_EVIDENCE={"run_id":"run","protocol":"mpp","method":"tempo",'
            '"chain_id":1337,"transaction":"0x' + 'ab' * 32 + '","amount":"100",'
            '"gas_fee":"7","gas_limit":1000,"gas_used":700,'
            '"payer_before":"1000","payer_after":"893",'
            f'"recipient_before":"10","recipient_after":"110","broadcasts":{broadcasts},'
            '"recovered_after_lost_response":true,"vault_handler_roundtrip":true,'
            '"encrypted_custody_signing":true,"outbound_receipt_reconciled":true,'
            f'"payer_ledger_entries":{payer_ledger_entries},"payee_ledger_entries":1,'
            '"settlement_requested_events":1}\n'
        )

    @patch("verifier.payment.s25_mpp_tempo.create_deployer")
    def test_verifies_recovery_and_exact_gas_accounting(self, create_deployer: Mock) -> None:
        create_deployer.return_value.mpp_tempo.return_value = self.evidence()

        evidence = verify(Mock())

        self.assertIn("type-0x76", evidence)
        self.assertIn("broadcasts=1", evidence)
        self.assertIn("payer/payee ledger", evidence)

    @patch("verifier.payment.s25_mpp_tempo.create_deployer")
    def test_rejects_duplicate_broadcast(self, create_deployer: Mock) -> None:
        create_deployer.return_value.mpp_tempo.return_value = self.evidence(broadcasts=2)

        with self.assertRaisesRegex(RuntimeError, "recovery evidence"):
            verify(Mock())

    @patch("verifier.payment.s25_mpp_tempo.create_deployer")
    def test_rejects_missing_payer_ledger_fact(self, create_deployer: Mock) -> None:
        create_deployer.return_value.mpp_tempo.return_value = self.evidence(payer_ledger_entries=0)

        with self.assertRaisesRegex(RuntimeError, "recovery evidence"):
            verify(Mock())


if __name__ == "__main__":
    unittest.main()
