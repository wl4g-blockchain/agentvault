import unittest

from agentvault.config import GrpcConfig, PayeeConfig, PayerConfig
from agentvault.model import AssetIdentity


class ConfigurationTest(unittest.TestCase):
    def test_role_configuration_validates_limits_and_redacts_edge_secrets(self):
        config = PayerConfig(
            "scenario", "wallet", "x402", AssetIdentity("eip155:1", "USDC", 6),
            "100", "recipient",
        )
        self.assertEqual(config.delivery_retries, 0)
        with self.assertRaises(ValueError):
            PayerConfig(
                "scenario", "wallet", "x402", AssetIdentity("eip155:1", "USDC", 6),
                "100", "recipient", delivery_retries=4,
            )
        with self.assertRaises(ValueError):
            PayeeConfig(0)
        grpc = GrpcConfig("vault.example:443", authguard_bearer="edge-secret")
        self.assertNotIn("edge-secret", repr(grpc))
