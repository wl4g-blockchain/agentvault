import unittest

from agentvault.config import MqttConfig
from agentvault.transport.mqtt import _subscription_granted


class MqttTransportTest(unittest.TestCase):
    def test_rejects_remote_cleartext(self):
        with self.assertRaises(ValueError):
            MqttConfig("broker.example", 1883, False, "user", "password", "worker", "client")

    def test_requires_exact_qos_one_subscription_grants(self):
        class Code:
            def __init__(self, value):
                self.value = value

        self.assertTrue(_subscription_granted([Code(1)], 1))
        self.assertFalse(_subscription_granted([Code(128)], 1))
        self.assertFalse(_subscription_granted([Code(1), Code(1)], 1))
