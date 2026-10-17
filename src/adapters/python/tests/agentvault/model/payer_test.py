import unittest

from agentvault.model import PayerRequestContext


class PayerModelTest(unittest.TestCase):
    def test_payer_context_is_stable(self):
        first = PayerRequestContext("run-1")
        retry = PayerRequestContext("run-1")
        self.assertEqual(first, retry)
        self.assertEqual(first.run_id, "run-1")
