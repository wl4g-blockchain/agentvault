import unittest

from agentvault.config import MqttConfig, VaultTransportConfig
from agentvault.transport import VaultTransportFactory


class FactoryTest(unittest.TestCase):
    def test_creates_isolated_idempotently_closable_transports(self):
        config = VaultTransportConfig("mqtt", mqtt=MqttConfig(
            "127.0.0.1", 1883, False, "client", "secret", "agent", "vault-client"))
        factory = VaultTransportFactory()
        first = factory.create(config)
        second = factory.create(config)
        self.assertIsNot(first, second)
        first.close()
        first.close()

        with self.assertRaises(ValueError):
            VaultTransportConfig("unknown")
