import unittest

from agentvault.agentvault import AgentVault
from agentvault.config import PayeeConfig, PayerConfig
from agentvault.model import AssetIdentity


class _Transport:
    def request(self, *_args, **_kwargs):
        return {}


class AgentVaultTest(unittest.TestCase):
    def test_builds_both_role_middlewares(self):
        vault = AgentVault(_Transport())
        payer = vault.payer(PayerConfig(
            "scenario", "wallet", "x402", AssetIdentity("eip155:31337", "USDC", 6),
            "1000", "0x0000000000000000000000000000000000000001"))
        self.assertIsNotNone(payer)
        self.assertIsNotNone(vault.payee(PayeeConfig()))
