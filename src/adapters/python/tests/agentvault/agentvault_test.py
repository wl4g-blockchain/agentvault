import unittest

from agentvault.agentvault import AgentVault
from agentvault.config import PayeeConfig, PayerConfig


class _Transport:
    def request(self, *_args, **_kwargs):
        return {}


class AgentVaultTest(unittest.TestCase):
    def test_builds_both_role_middlewares(self):
        vault = AgentVault(_Transport())
        payer = vault.payer(PayerConfig("scenario"))
        self.assertIsNotNone(payer)
        self.assertIsNotNone(vault.payee(PayeeConfig()))
