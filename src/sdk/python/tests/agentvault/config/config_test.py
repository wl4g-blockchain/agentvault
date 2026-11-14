import unittest

from agentvault.sdk.config import HttpConfig, PayeeConfig, PayerConfig


class ConfigurationTest(unittest.TestCase):
    def test_role_configuration_validates_limits_and_redacts_edge_secrets(self):
        config = PayerConfig("scenario")
        self.assertEqual(config.delivery_retries, 0)
        with self.assertRaises(ValueError):
            PayerConfig("scenario", delivery_retries=4)
        with self.assertRaises(ValueError):
            PayeeConfig(0)
        http = HttpConfig("https://vault.example", authguard_bearer="edge-secret")
        self.assertNotIn("edge-secret", repr(http))
