import unittest
from agentvault.sdk.config import HttpConfig, VaultTransportConfig
from agentvault.sdk.transport import VaultTransportFactory


class FactoryTest(unittest.TestCase):
    def test_creates_isolated_idempotently_closable_transports(self):
        config = VaultTransportConfig("http", HttpConfig("http://127.0.0.1:65535"))
        first = VaultTransportFactory().create(config)
        second = VaultTransportFactory().create(config)
        self.assertIsNot(first, second)
        first.close()
        first.close()
        second.close()
        with self.assertRaises(ValueError):
            VaultTransportConfig("unknown", config.http)
