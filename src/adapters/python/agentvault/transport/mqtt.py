from __future__ import annotations

import base64
import binascii
import json
import re
import secrets
import ssl
import threading
import time
from collections import OrderedDict
from typing import Any, Mapping

import paho.mqtt.client as mqtt

from ..config import MqttConfig
from .http import APIError

_CORRELATION = re.compile(r"^[0-9a-f]{32}$")
_AUTHORIZE_PATH = "/api/v1/payments/authorize"
_OUTBOUND_RESPONSE_PATH = re.compile(r"^/api/v1/payments/([^/?#]+)/outbound-response$")


class MqttVaultTransport:
    """Authenticated HTTP-over-MQTT transport for payer and payee flows."""

    def __init__(self, config: MqttConfig) -> None:
        self.config = config
        self._condition = threading.Condition()
        self._connected = False
        self._connection_error: RuntimeError | None = None
        self._responses: OrderedDict[str, Mapping[str, Any]] = OrderedDict()
        self._expected_types: dict[str, str] = {}
        connection_id = f"{config.connection_prefix}-{secrets.token_hex(6)}"
        self._client = mqtt.Client(mqtt.CallbackAPIVersion.VERSION2, client_id=connection_id,
                                   protocol=mqtt.MQTTv311, clean_session=True)
        self._client.username_pw_set(config.username, config.password)
        if config.tls:
            self._client.tls_set(cert_reqs=ssl.CERT_REQUIRED)
        self._client.on_connect = self._on_connect
        self._client.on_subscribe = self._on_subscribe
        self._client.on_message = self._on_message
        self._prefix = f"agentvault/v1/clients/{config.vault_client_id}"

    def __repr__(self) -> str:
        return f"MqttVaultTransport(config={self.config!r}, state=<redacted>)"

    def connect(self) -> None:
        self._client.connect(self.config.host, self.config.port, keepalive=15)
        self._client.loop_start()
        deadline = time.monotonic() + self.config.timeout
        with self._condition:
            while not self._connected and self._connection_error is None:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    self.close()
                    raise TimeoutError("MQTT connect/subscribe timed out")
                self._condition.wait(remaining)
            if self._connection_error is not None:
                error = self._connection_error
                self.close()
                raise error

    def close(self) -> None:
        try:
            self._client.disconnect()
        finally:
            self._client.loop_stop()

    def __enter__(self) -> "MqttVaultTransport":
        self.connect()
        return self

    def __exit__(self, *_: object) -> None:
        self.close()

    def request(self, method: str, path: str, body: Mapping[str, Any] | None = None) -> dict[str, Any]:
        operation = self._operation(method, path)
        if body is None:
            raise ValueError("MQTT outbound payment commands require a request body")
        correlation = secrets.token_hex(16)
        encoded_body = json.dumps(body, separators=(",", ":"), ensure_ascii=False).encode()
        headers = {"accept": "application/json", "content-type": "application/json"}
        envelope = {
            "type": operation[1], "version": 1, "correlationId": correlation,
            "request": {"method": "POST", "path": path, "headers": headers,
                        "body": base64.urlsafe_b64encode(encoded_body).rstrip(b"=").decode("ascii")},
        }
        raw = json.dumps(envelope, separators=(",", ":")).encode()
        if len(raw) > self.config.max_response_bytes:
            raise ValueError("MQTT outbound payment request exceeds configured limit")
        deadline = time.monotonic() + self.config.timeout
        with self._condition:
            self._expected_types[correlation] = operation[2]
        info = self._client.publish(f"{self._prefix}/commands/{operation[0]}", raw, qos=1, retain=False)
        if info.rc != mqtt.MQTT_ERR_SUCCESS:
            with self._condition:
                self._expected_types.pop(correlation, None)
            raise RuntimeError("MQTT authorization publish failed")
        info.wait_for_publish(timeout=self.config.timeout)
        if not info.is_published():
            raise TimeoutError("MQTT authorization publish timed out")
        try:
            with self._condition:
                while correlation not in self._responses:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TimeoutError("AgentVault MQTT request timed out")
                    self._condition.wait(remaining)
                response = self._responses.pop(correlation)
        finally:
            with self._condition:
                self._expected_types.pop(correlation, None)
        status = response["status"]
        response_body = self._decode_body(response["body"])
        try:
            value = json.loads(response_body)
        except (UnicodeDecodeError, ValueError):
            raise RuntimeError("invalid AgentVault MQTT JSON response") from None
        if not isinstance(value, dict):
            raise RuntimeError("AgentVault MQTT response body must be a JSON object")
        if not 200 <= status < 300:
            detail = value.get("error", {})
            if not isinstance(detail, dict):
                detail = {}
            raise APIError(status, str(detail.get("code", "HTTP_ERROR")),
                           str(detail.get("message", f"AgentVault returned HTTP {status}")))
        return value

    @staticmethod
    def _operation(method: str, path: str) -> tuple[str, str, str]:
        if method == "POST" and path == _AUTHORIZE_PATH:
            return ("authorize", "payment.authorize", "payment.authorization_result")
        if method == "POST" and _OUTBOUND_RESPONSE_PATH.fullmatch(path):
            return ("outbound-response", "payment.outbound_response",
                    "payment.outbound_response_result")
        payee = {
            "/api/v1/offers/resolve":
                ("resolve-offer", "payment.resolve_offer", "payment.offer_resolved"),
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
        result, _ = client.subscribe(f"{self._prefix}/results/+", qos=1)
        with self._condition:
            if result != mqtt.MQTT_ERR_SUCCESS:
                self._connection_error = RuntimeError("AgentVault MQTT subscription could not be sent")
                self._condition.notify_all()

    def _on_subscribe(self, _client: mqtt.Client, _userdata: Any, _mid: int,
                      reason_codes: list[Any], _properties: Any) -> None:
        with self._condition:
            if not _subscription_granted(reason_codes, 1):
                self._connection_error = RuntimeError(
                    "AgentVault MQTT subscriptions were rejected or downgraded")
            else:
                self._connected = True
            self._condition.notify_all()

    def _on_message(self, _client: mqtt.Client, _userdata: Any, message: mqtt.MQTTMessage) -> None:
        if len(message.payload) > self.config.max_response_bytes:
            return
        try:
            value = json.loads(message.payload)
        except (ValueError, UnicodeDecodeError):
            return
        if not isinstance(value, dict) or value.get("version") != 1:
            return
        correlation = value.get("correlationId")
        expected_type = self._expected_types.get(correlation) if isinstance(correlation, str) else None
        status = value.get("status")
        response_type = value.get("type")
        type_matches = (isinstance(status, int) and isinstance(response_type, str)
                        and ((status >= 400 and response_type == "payment.error")
                             or (status < 400 and response_type == expected_type)))
        if (expected_type is None or not type_matches or not isinstance(correlation, str)
                or not _CORRELATION.fullmatch(correlation)
                or message.topic != f"{self._prefix}/results/{correlation}"
                or set(value) != {"type", "version", "correlationId", "status", "headers", "body"}
                or not 100 <= status <= 599
                or not isinstance(value.get("headers"), dict)
                or not all(isinstance(key, str) and isinstance(item, str) for key, item in value["headers"].items())
                or not isinstance(value.get("body"), str)):
            return
        with self._condition:
            if correlation not in self._responses:
                self._responses[correlation] = value
                while len(self._responses) > 256:
                    self._responses.popitem(last=False)
            self._condition.notify_all()


def _subscription_granted(reason_codes: list[Any], expected_count: int) -> bool:
    return (len(reason_codes) == expected_count
            and all(getattr(code, "value", code) == 1 for code in reason_codes))
