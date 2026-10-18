import unittest
from unittest.mock import Mock

from agentvault.config import MqttConfig
from agentvault.transport.mqtt import (
    MqttVaultTransport, _producer_trace_context, _subscription_granted,
)


class MqttTransportTest(unittest.TestCase):
    @staticmethod
    def _config():
        return MqttConfig("127.0.0.1", 1883, False, "user", "password", "worker", "client")

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

    def test_request_span_id_is_the_w3c_producer_span(self):
        parent = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01"
        trace_id, span_id, parent_span_id, traceparent = _producer_trace_context({"traceparent": parent})
        self.assertEqual(trace_id, "0123456789abcdef0123456789abcdef")
        self.assertEqual(parent_span_id, "0123456789abcdef")
        self.assertEqual(traceparent.split("-")[2], span_id)

    def test_request_lazily_connects_transport(self):
        transport = MqttVaultTransport(self._config())
        transport.connect = Mock(side_effect=RuntimeError("lazy-connect"))

        with self.assertRaisesRegex(RuntimeError, "lazy-connect"):
            transport.request("POST", "/api/v1/payee/challenges", {"runId": "run-1"})

        transport.connect.assert_called_once_with()

    def test_connect_is_idempotent_after_subscription(self):
        transport = MqttVaultTransport(self._config())
        transport._connected = True
        transport._client.connect = Mock()

        transport.connect()

        transport._client.connect.assert_not_called()
