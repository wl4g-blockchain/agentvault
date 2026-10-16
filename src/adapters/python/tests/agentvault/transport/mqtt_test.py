import unittest

from agentvault.config import MqttConfig


class MqttTransportTest(unittest.TestCase):
    def test_rejects_remote_cleartext(self):
        with self.assertRaises(ValueError):
            MqttConfig("broker.example", 1883, False, "user", "password", "worker", "client")
