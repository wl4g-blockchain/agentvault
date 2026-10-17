import unittest

from agentvault.model import HttpCredential, OperationContext, PaymentCredential


class BaseModelTest(unittest.TestCase):
    def test_base_models_enforce_safe_run_identity_and_redact_credentials(self):
        context = OperationContext("scenario", "run-1")
        self.assertEqual(context.run_id, "run-1")
        with self.assertRaises(ValueError):
            OperationContext("scenario", "unsafe/key")
        credential = PaymentCredential(
            "x402", "challenge-secret", "credential-secret", {"signature": "signature-secret"},
            HttpCredential("https://merchant.test", "PAYMENT-SIGNATURE", "header-secret"),
        )
        rendered = repr(credential)
        for secret in ("challenge-secret", "credential-secret", "signature-secret", "header-secret"):
            self.assertNotIn(secret, rendered)
