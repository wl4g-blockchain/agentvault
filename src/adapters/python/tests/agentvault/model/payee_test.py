import unittest

from agentvault.model import PayeeOperation


class PayeeModelTest(unittest.TestCase):
    def test_payee_phases_reuse_one_run_identity(self):
        operation = PayeeOperation(
            "scenario", "run", "offer", "mcp://marketpulse/realtime",
            "https://marketpulse.test", "x402", "hash",
        )
        self.assertEqual(operation.challenge_context().run_id, "run")
        self.assertEqual(operation.settlement_context().run_id, "run")
