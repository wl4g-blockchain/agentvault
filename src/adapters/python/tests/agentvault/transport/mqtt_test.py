import base64
import json
import socket
import threading
import unittest
from concurrent.futures import Future
from unittest.mock import Mock

from agentvault.config import MqttConfig
from agentvault.transport.mqtt import MqttVaultTransport


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

        self.assertTrue(MqttVaultTransport._subscription_granted([Code(1)], 1))
        self.assertFalse(MqttVaultTransport._subscription_granted([Code(128)], 1))
        self.assertFalse(MqttVaultTransport._subscription_granted([Code(1), Code(1)], 1))

    def test_request_span_id_is_the_w3c_producer_span(self):
        parent = "00-0123456789abcdef0123456789abcdef-0123456789abcdef-01"
        trace_id, span_id, parent_span_id, traceparent = MqttVaultTransport._producer_trace_context({"traceparent": parent})
        self.assertEqual(trace_id, "0123456789abcdef0123456789abcdef")
        self.assertEqual(parent_span_id, "0123456789abcdef")
        self.assertEqual(traceparent.split("-")[2], span_id)

    def test_request_lazily_connects_transport(self):
        transport = MqttVaultTransport(self._config())
        transport._start = Mock(side_effect=RuntimeError("lazy-connect"))

        with self.assertRaisesRegex(RuntimeError, "lazy-connect"):
            transport.request("POST", "/api/v1/payee/challenges", {"runId": "run-1"})

        transport._start.assert_called_once_with()

    def test_async_request_is_completed_by_response_callback(self):
        transport = MqttVaultTransport(self._config())
        transport._start = Mock()
        transport._publish_pending = Mock()

        future = transport.request_async(
            "POST", "/api/v1/payee/challenges", {"runId": "run-1"})

        self.assertFalse(future.done())
        request_span_id = next(iter(transport._pending))
        body = base64.urlsafe_b64encode(json.dumps({"ok": True}).encode()).rstrip(b"=").decode()
        transport._complete(request_span_id, {
            "type": "payment.challenge_created",
            "version": 1,
            "requestSpanId": request_span_id,
            "status": 200,
            "headers": {},
            "body": body,
        })

        self.assertEqual(future.result(timeout=0.1), {"ok": True})
        transport.close()

    def test_connect_is_idempotent_after_subscription(self):
        transport = MqttVaultTransport(self._config())
        transport._connected = True
        transport._client.connect = Mock()

        transport.connect()

        transport._client.connect.assert_not_called()

    def test_cancel_and_close_remove_pending_replays(self):
        transport = MqttVaultTransport(self._config())
        transport._start = Mock()
        cancelled = transport.request_async("POST", "/api/v1/payee/challenges", {"runId": "cancel"})
        self.assertTrue(cancelled.cancel())
        self.assertEqual(transport._pending, {})
        waiting = transport.request_async("POST", "/api/v1/payee/challenges", {"runId": "close"})
        transport.close()
        transport.close()
        with self.assertRaisesRegex(RuntimeError, "closed"):
            waiting.result(timeout=0.1)
        self.assertEqual(transport._pending, {})

    def test_real_tcp_reconnect_replays_qos_one_envelopes_and_pairs_reordered_replies(self):
        with _MqttPeer(self) as peer:
            config = MqttConfig("127.0.0.1", peer.port, False, "user", "password", "worker", "client", timeout=8)
            transport = MqttVaultTransport(config)
            self.addCleanup(transport.close)
            first = transport.request_async("POST", "/api/v1/payee/challenges", {"runId": "one"})
            second = transport.request_async("POST", "/api/v1/payee/challenges", {"runId": "two"})
            self.assertEqual(first.result(timeout=10), {"runId": "one"})
            self.assertEqual(second.result(timeout=10), {"runId": "two"})
            peer.outcome.result(timeout=2)
            self.assertEqual(transport._pending, {})
            transport.close()


class _MqttPeer:
    """A deterministic MQTT 3.1.1 peer that drops an acknowledged command's reply."""

    def __init__(self, test):
        self.test = test
        self.listener = socket.socket()
        self.listener.bind(("127.0.0.1", 0))
        self.listener.listen(2)
        self.listener.settimeout(10)
        self.port = self.listener.getsockname()[1]
        self.outcome = Future()
        self.thread = threading.Thread(target=self._run, daemon=True)

    def __enter__(self):
        self.thread.start()
        return self

    def __exit__(self, *_):
        self.listener.close()
        self.thread.join(timeout=2)

    @staticmethod
    def _read(sock, size):
        result = b""
        while len(result) < size:
            chunk = sock.recv(size - len(result))
            if not chunk:
                raise EOFError("MQTT client disconnected")
            result += chunk
        return result

    @classmethod
    def _packet(cls, sock):
        header = cls._read(sock, 1)[0]
        size, multiplier = 0, 1
        for _ in range(4):
            digit = cls._read(sock, 1)[0]
            size += (digit & 127) * multiplier
            if not digit & 128:
                return header, cls._read(sock, size)
            multiplier *= 128
        raise ValueError("invalid MQTT remaining length")

    @staticmethod
    def _send(sock, header, payload):
        size, length = len(payload), bytearray()
        while True:
            digit, size = size % 128, size // 128
            length.append(digit | (128 if size else 0))
            if not size:
                break
        sock.sendall(bytes([header]) + length + payload)

    def _commands(self, sock):
        kind, _ = self._packet(sock)
        self.test.assertEqual(kind >> 4, 1)
        self._send(sock, 0x20, b"\0\0")
        kind, subscribe = self._packet(sock)
        self.test.assertEqual(kind, 0x82)
        self.test.assertEqual(subscribe[-1], 1)
        self._send(sock, 0x90, subscribe[:2] + b"\1")
        commands = {}
        while len(commands) < 2:
            kind, packet = self._packet(sock)
            self.test.assertEqual(kind & 0xF7, 0x32)  # QoS 1, optional DUP.
            topic_size = int.from_bytes(packet[:2], "big")
            packet_id = packet[2 + topic_size:4 + topic_size]
            raw = packet[4 + topic_size:]
            envelope = json.loads(raw)
            span = envelope["requestSpanId"]
            self.test.assertEqual(span, envelope["request"]["headers"]["traceparent"].split("-")[2])
            commands[span] = raw
            self._send(sock, 0x40, packet_id)
        return commands

    def _run(self):
        try:
            with self.listener.accept()[0] as first:
                first.settimeout(8)
                original = self._commands(first)
            with self.listener.accept()[0] as second:
                second.settimeout(8)
                replay = self._commands(second)
                self.test.assertEqual(original, replay)
                for index, (span, raw) in enumerate(reversed(list(original.items())), 1):
                    envelope = json.loads(raw)
                    reply = json.dumps({"type": "payment.challenge_created", "version": 1,
                                        "requestSpanId": span, "status": 200, "headers": {},
                                        "body": envelope["request"]["body"]}).encode()
                    topic = f"agentvault/v1/clients/client/results/{span}".encode()
                    payload = len(topic).to_bytes(2, "big") + topic + index.to_bytes(2, "big") + reply
                    self._send(second, 0x32, payload)
                    self._send(second, 0x3A, payload)  # Duplicate reply must complete once.
                acknowledgements = set()
                while acknowledgements != {1, 2}:
                    kind, packet = self._packet(second)
                    if kind == 0x40:
                        acknowledgements.add(int.from_bytes(packet, "big"))
                    elif kind & 0xF7 == 0x32:
                        topic_size = int.from_bytes(packet[:2], "big")
                        self.test.assertIn(packet[4 + topic_size:], original.values())
                        self._send(second, 0x40, packet[2 + topic_size:4 + topic_size])
                    else:
                        raise AssertionError(f"unexpected MQTT packet {kind}")
            self.outcome.set_result(None)
        except BaseException as error:
            self.outcome.set_exception(error)
