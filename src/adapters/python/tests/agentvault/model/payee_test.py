import unittest

from agentvault.model import PayeeOperation


class PayeeModelTest(unittest.TestCase):
    def test_payee_phase_keys_are_bound_to_one_execution(self):
        operation = PayeeOperation(
            "scenario", "run", "run.realtime", "offer", "mcp://marketpulse/realtime",
            "https://marketpulse.test", "x402", "hash",
        )
        self.assertEqual(operation.challenge_context().idempotency_key, "run.realtime.challenge")
        self.assertEqual(operation.settlement_context().idempotency_key, "run.realtime.settle")
