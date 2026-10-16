import unittest

from agentvault.model import PayerRequestContext


class PayerModelTest(unittest.TestCase):
    def test_payer_context_is_stable(self):
        first = PayerRequestContext.for_operation("run-1", "realtime")
        retry = PayerRequestContext.for_operation("run-1", "realtime")
        self.assertEqual(first, retry)
        self.assertEqual(first.idempotency_key, "run-1:realtime")
