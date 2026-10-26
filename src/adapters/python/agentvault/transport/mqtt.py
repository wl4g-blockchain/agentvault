from __future__ import annotations

import base64
import binascii
import json
import logging
import re
import secrets
import ssl
import threading
import time
from concurrent.futures import Future
from dataclasses import dataclass
from typing import Any, Mapping

import paho.mqtt.client as mqtt

from ..config import MqttConfig
from .transport import APIError

_REQUEST_SPAN_ID = re.compile(r"^(?!0{16}$)[0-9a-f]{16}$")
_AUTHORIZE_PATH = "/api/v1/payments/authorize"
_OUTBOUND_RESPONSE_PATH = re.compile(r"^/api/v1/payments/([^/?#]+)/outbound-response$")
_TRACEPARENT = re.compile(
    r"^00-((?!0{32})[0-9a-f]{32})-((?!0{16})[0-9a-f]{16})-([0-9a-f]{2})$")
_LOG = logging.getLogger("agentvault.sdk.transport.mqtt")


@dataclass(frozen=True)
class _PendingRequest:
    expected_type: str
    command_topic: str
    payload: bytes
    future: Future[dict[str, Any]]
    timer: threading.Timer


class MqttVaultTransport:
    """Authenticated request-envelope MQTT transport for payer and payee flows."""

    def __init__(self, config: MqttConfig) -> None:
        self.config = config
        self._condition = threading.Condition()
        self._lifecycle_lock = threading.RLock()
        self._connected = False
        self._loop_started = False
        self._closed = False
        self._connection_error: RuntimeError | None = None
        self._pending: dict[str, _PendingRequest] = {}
        connection_id = f"{config.connection_prefix}-{secrets.token_hex(6)}"
        self._client = mqtt.Client(mqtt.CallbackAPIVersion.VERSION2, client_id=connection_id,
                                   protocol=mqtt.MQTTv311, clean_session=True)
        self._client.username_pw_set(config.username, config.password)
        if config.tls:
            self._client.tls_set(cert_reqs=ssl.CERT_REQUIRED)
        self._client.on_connect = self._on_connect
        self._client.on_subscribe = self._on_subscribe
        self._client.on_disconnect = self._on_disconnect
        self._client.on_message = self._on_message
        self._prefix = f"agentvault/v1/clients/{config.vault_client_id}"

    def __repr__(self) -> str:
        return f"MqttVaultTransport(config={self.config!r}, state=<redacted>)"

    def connect(self) -> None:
        self._start()
        deadline = time.monotonic() + self.config.timeout
        with self._condition:
            while not self._connected and self._connection_error is None and not self._closed:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise TimeoutError("MQTT connect/subscribe timed out")
                self._condition.wait(remaining)
            if self._connection_error is not None:
                raise self._connection_error
            if self._closed:
                raise RuntimeError("MQTT transport closed")

    def _start(self) -> None:
        with self._lifecycle_lock:
            if self._closed:
                raise RuntimeError("AgentVault MQTT transport is closed")
            if self._loop_started or self._connected:
                return
            self._client.reconnect_delay_set(min_delay=1, max_delay=max(1, int(self.config.timeout)))
            self._client.connect_async(self.config.host, self.config.port, keepalive=15)
            self._client.loop_start()
            self._loop_started = True

    def close(self) -> None:
        with self._lifecycle_lock:
            if self._closed:
                return
            with self._condition:
                self._connected = False
                self._closed = True
                pending = list(self._pending.values())
                self._pending.clear()
                self._condition.notify_all()
            loop_started = self._loop_started
        # Never join the network loop while holding a lock needed by callbacks.
        for waiting in pending:
            waiting.timer.cancel()
            if waiting.future.set_running_or_notify_cancel():
                waiting.future.set_exception(RuntimeError("MQTT transport closed"))
        if loop_started:
            self._client.disconnect()
            self._client.loop_stop()

    def __enter__(self) -> "MqttVaultTransport":
        self.connect()
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def request(self, method: str, path: str, body: Mapping[str, Any] | None = None) -> dict[str, Any]:
        return self.request_async(method, path, body).result()

    def request_async(
        self,
        method: str,
        path: str,
        body: Mapping[str, Any] | None = None,
    ) -> Future[dict[str, Any]]:
        """Submits a QoS 1 command and completes it from MQTT callbacks.

        Reconnect recovery republishes the same serialized envelope so Vault
        can coalesce duplicates by requestSpanId and payload digest.
        """
        future: Future[dict[str, Any]] = Future()
        operation = self._operation(method, path)
        if body is None:
            raise ValueError("MQTT outbound payment commands require a request body")
        trace_id, request_span_id, parent_span_id, traceparent = self._producer_trace_context({})
        encoded_body = json.dumps(body, separators=(",", ":"), ensure_ascii=False).encode()
        headers = {
            "accept": "application/json",
            "content-type": "application/json",
            "traceparent": traceparent,
        }
        _LOG.info(
            "agentvault.sdk.transport.mqtt.produced trace_id=%s span_id=%s parent_id=%s",
            trace_id, request_span_id, parent_span_id or "",
            extra={"event_name": "agentvault.sdk.transport.mqtt.produced",
                   "trace_id": trace_id, "span_id": request_span_id,
                   "parent_id": parent_span_id or ""},
        )
        envelope = {
            "type": operation[1], "version": 1, "requestSpanId": request_span_id,
            "request": {"method": "POST", "path": path, "headers": headers,
                        "body": base64.urlsafe_b64encode(encoded_body).rstrip(b"=").decode("ascii")},
        }
        raw = json.dumps(envelope, separators=(",", ":")).encode()
        if len(raw) > self.config.max_response_bytes:
            raise ValueError("MQTT outbound payment request exceeds configured limit")
        command_topic = f"{self._prefix}/commands/{operation[0]}"
        timer = threading.Timer(self.config.timeout, self._timeout, args=(request_span_id,))
        timer.daemon = True
        waiting = _PendingRequest(operation[2], command_topic, raw, future, timer)
        with self._condition:
            if self._closed:
                raise RuntimeError("AgentVault MQTT transport is closed")
            if len(self._pending) >= 256:
                raise RuntimeError("too many pending MQTT payment requests")
            if request_span_id in self._pending:
                raise RuntimeError("MQTT requestSpanId collision")
            self._pending[request_span_id] = waiting
            ready = self._connected
        future.add_done_callback(lambda completed: self._cancel(request_span_id, completed))
        try:
            self._start()
        except Exception as error:
            with self._condition:
                removed = self._pending.pop(request_span_id, None)
            if removed is not None and future.set_running_or_notify_cancel():
                future.set_exception(error)
            return future
        timer.start()
        if ready:
            self._publish_pending(request_span_id)
        return future

    def _complete(self, request_span_id: str, response: Mapping[str, Any]) -> None:
        with self._condition:
            waiting = self._pending.pop(request_span_id, None)
        if waiting is None:
            return
        waiting.timer.cancel()
        if not waiting.future.set_running_or_notify_cancel():
            return
        try:
            status = response["status"]
            response_body = self._decode_body(response["body"])
            value = json.loads(response_body)
        except (KeyError, UnicodeDecodeError, ValueError, RuntimeError):
            waiting.future.set_exception(RuntimeError("invalid AgentVault MQTT JSON response"))
            return
        if not isinstance(value, dict):
            waiting.future.set_exception(RuntimeError("AgentVault MQTT response body must be a JSON object"))
            return
        if not 200 <= status < 300:
            detail = value.get("error", {})
            if not isinstance(detail, dict):
                detail = {}
            waiting.future.set_exception(APIError(
                status,
                str(detail.get("code", "TRANSPORT_ERROR")),
                str(detail.get("message", f"AgentVault returned status {status}")),
            ))
            return
        waiting.future.set_result(value)

    def _timeout(self, request_span_id: str) -> None:
        with self._condition:
            waiting = self._pending.pop(request_span_id, None)
        if waiting is not None and waiting.future.set_running_or_notify_cancel():
            waiting.future.set_exception(TimeoutError("AgentVault MQTT request timed out"))

    def _cancel(self, request_span_id: str, future: Future[dict[str, Any]]) -> None:
        if not future.cancelled():
            return
        with self._condition:
            waiting = self._pending.pop(request_span_id, None)
        if waiting is not None:
            waiting.timer.cancel()

    def _publish_pending(self, request_span_id: str) -> None:
        with self._condition:
            waiting = self._pending.get(request_span_id)
            ready = self._connected and not self._closed
        if waiting is None or not ready:
            return
        info = self._client.publish(waiting.command_topic, waiting.payload, qos=1, retain=False)
        if info.rc not in (mqtt.MQTT_ERR_SUCCESS, mqtt.MQTT_ERR_NO_CONN):
            _LOG.warning("AgentVault MQTT publish deferred until reconnect rc=%s", info.rc)

    @staticmethod
    def _operation(method: str, path: str) -> tuple[str, str, str]:
        if method == "POST" and path == _AUTHORIZE_PATH:
            return ("authorize", "payment.authorize", "payment.authorization_result")
        if method == "POST" and _OUTBOUND_RESPONSE_PATH.fullmatch(path):
            return ("outbound-response", "payment.outbound_response",
                    "payment.outbound_response_result")
        payee = {
            "/api/v1/prices/resolve":
                ("resolve-price", "payment.resolve_price", "payment.price_resolved"),
            "/api/v1/payee/challenges":
                ("create-challenge", "payment.create_challenge", "payment.challenge_created"),
            "/api/v1/payee/verify":
                ("verify-payment", "payment.verify", "payment.verification_result"),
            "/api/v1/payee/settle":
                ("settle-payment", "payment.settle", "payment.settlement_result"),
        }
        if method == "POST" and path in payee:
            return payee[path]
        raise ValueError("MQTT transport does not support this payment operation")

    def _decode_body(self, value: Any) -> bytes:
        if not isinstance(value, str) or not re.fullmatch(r"[A-Za-z0-9_-]*", value):
            raise RuntimeError("invalid AgentVault MQTT response body")
        try:
            decoded = base64.b64decode(value + "=" * (-len(value) % 4), altchars=b"-_", validate=True)
        except (binascii.Error, ValueError):
            raise RuntimeError("invalid AgentVault MQTT response body") from None
        canonical = base64.urlsafe_b64encode(decoded).rstrip(b"=").decode("ascii")
        if canonical != value or len(decoded) > self.config.max_response_bytes:
            raise RuntimeError("invalid or oversized AgentVault MQTT response body")
        return decoded

    def _on_connect(self, client: mqtt.Client, _userdata: Any, _flags: Any,
                    reason_code: Any, _properties: Any) -> None:
        if reason_code != 0:
            with self._condition:
                self._connection_error = RuntimeError("AgentVault MQTT connection was rejected")
                self._condition.notify_all()
            return
        with self._condition:
            self._connected = False
            self._connection_error = None
        result, _ = client.subscribe(f"{self._prefix}/results/+", qos=1)
        with self._condition:
            if result != mqtt.MQTT_ERR_SUCCESS:
                self._connection_error = RuntimeError("AgentVault MQTT subscription could not be sent")
                self._condition.notify_all()

    def _on_subscribe(self, _client: mqtt.Client, _userdata: Any, _mid: int,
                      reason_codes: list[Any], _properties: Any) -> None:
        with self._condition:
            if self._closed:
                return
            if not self._subscription_granted(reason_codes, 1):
                self._connection_error = RuntimeError(
                    "AgentVault MQTT subscriptions were rejected or downgraded")
            else:
                self._connected = True
                pending_ids = tuple(self._pending)
            self._condition.notify_all()
        if self._connected:
            for request_span_id in pending_ids:
                self._publish_pending(request_span_id)

    def _on_disconnect(self, _client: mqtt.Client, _userdata: Any, _flags: Any,
                       _reason_code: Any, _properties: Any) -> None:
        with self._condition:
            self._connected = False
            self._condition.notify_all()

    def _on_message(self, _client: mqtt.Client, _userdata: Any, message: mqtt.MQTTMessage) -> None:
        if message.qos != 1 or message.retain or len(message.payload) > self.config.max_response_bytes:
            return
        try:
            value = json.loads(message.payload)
        except (ValueError, UnicodeDecodeError):
            return
        if not isinstance(value, dict) or type(value.get("version")) is not int or value["version"] != 1:
            return
        request_span_id = value.get("requestSpanId")
        with self._condition:
            waiting = self._pending.get(request_span_id) if isinstance(request_span_id, str) else None
        expected_type = waiting.expected_type if waiting is not None else None
        status = value.get("status")
        response_type = value.get("type")
        type_matches = (type(status) is int and isinstance(response_type, str)
                        and ((status >= 400 and response_type == "payment.error")
                             or (status < 400 and response_type == expected_type)))
        if (expected_type is None or not type_matches or not isinstance(request_span_id, str)
                or not _REQUEST_SPAN_ID.fullmatch(request_span_id)
                or message.topic != f"{self._prefix}/results/{request_span_id}"
                or set(value) != {"type", "version", "requestSpanId", "status", "headers", "body"}
                or not 100 <= status <= 599
                or not isinstance(value.get("headers"), dict)
                or not all(isinstance(key, str) and isinstance(item, str) for key, item in value["headers"].items())
                or not isinstance(value.get("body"), str)):
            return
        try:
            self._complete(request_span_id, value)
        except RuntimeError as error:
            with self._condition:
                current = self._pending.pop(request_span_id, None)
            if current is not None:
                current.timer.cancel()
                current.future.set_exception(error)


    @staticmethod
    def _subscription_granted(reason_codes: list[Any], expected_count: int) -> bool:
        return (len(reason_codes) == expected_count
                and all(getattr(code, "value", code) == 1 for code in reason_codes))

    @classmethod
    def _producer_trace_context(cls, headers: Mapping[str, str]) -> tuple[str, str, str | None, str]:
        parent = next((value for name, value in headers.items() if name.lower() == "traceparent"), "")
        match = _TRACEPARENT.fullmatch(parent)
        trace_id = match.group(1) if match else cls._random_nonzero_hex(16)
        parent_span_id = match.group(2) if match else None
        flags = match.group(3) if match else "01"
        span_id = cls._random_nonzero_hex(8)
        return trace_id, span_id, parent_span_id, f"00-{trace_id}-{span_id}-{flags}"

    @staticmethod
    def _random_nonzero_hex(size: int) -> str:
        while True:
            value = secrets.token_hex(size)
            if set(value) != {"0"}:
                return value
