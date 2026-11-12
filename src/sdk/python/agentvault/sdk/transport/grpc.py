from __future__ import annotations

import base64
import json
import logging
import secrets
import threading
from typing import Any, Mapping

import grpc
from google.protobuf.wrappers_pb2 import BytesValue

from ..config import GrpcConfig
from .transport import APIError
_EXECUTE = "/agentvault.transport.v1.PaymentTransport/Execute"
_LOG = logging.getLogger("agentvault.sdk.transport.grpc")


class GrpcVaultTransport:
    """Bounded synchronous gRPC transport shared by payer and payee clients."""

    def __init__(self, config: GrpcConfig) -> None:
        self.config = config
        options = (("grpc.max_send_message_length", config.max_message_bytes),
                   ("grpc.max_receive_message_length", config.max_message_bytes))
        if config.tls:
            self._channel = grpc.secure_channel(config.endpoint, grpc.ssl_channel_credentials(), options=options)
        else:
            self._channel = grpc.insecure_channel(config.endpoint, options=options)
        self._execute = self._channel.unary_unary(
            _EXECUTE,
            request_serializer=BytesValue.SerializeToString,
            response_deserializer=BytesValue.FromString,
        )
        self._close_lock = threading.Lock()
        self._closed = False

    def __repr__(self) -> str:
        return f"GrpcVaultTransport(config={self.config!r}, channel=<redacted>)"

    def request(self, method: str, path: str, body: Mapping[str, Any] | None = None) -> dict[str, Any]:
        if method not in ("GET", "POST") or not path.startswith("/api/v1/") or "\\" in path:
            raise ValueError("invalid AgentVault gRPC request")
        encoded_body = b"" if body is None else json.dumps(
            body, separators=(",", ":"), ensure_ascii=False).encode()
        headers = {"accept": "application/json"}
        if body is not None:
            headers["content-type"] = "application/json"
        envelope = json.dumps({
            "method": method,
            "path": path,
            "headers": headers,
            "body": base64.urlsafe_b64encode(encoded_body).rstrip(b"=").decode("ascii"),
        }, separators=(",", ":")).encode()
        if len(envelope) > self.config.max_message_bytes:
            raise ValueError("AgentVault gRPC request exceeds configured limit")
        trace_id, span_id = secrets.token_hex(16), secrets.token_hex(8)
        while int(trace_id, 16) == 0:
            trace_id = secrets.token_hex(16)
        while int(span_id, 16) == 0:
            span_id = secrets.token_hex(8)
        parent_span_id = None
        traceparent = f"00-{trace_id}-{span_id}-01"
        metadata: tuple[tuple[str, str], ...] = (("traceparent", traceparent),)
        if self.config.authguard_bearer:
            metadata += (("authorization", f"Bearer {self.config.authguard_bearer}"),)
        _LOG.info(
            "agentvault.sdk.transport.grpc.produced trace_id=%s span_id=%s parent_id=%s",
            trace_id, span_id, parent_span_id or "",
            extra={"event_name": "agentvault.sdk.transport.grpc.produced",
                   "trace_id": trace_id, "span_id": span_id,
                   "parent_id": parent_span_id or ""},
        )
        try:
            response: BytesValue = self._execute(BytesValue(value=envelope), timeout=self.config.timeout,
                                                 metadata=metadata)
        except grpc.RpcError as error:
            raise RuntimeError(f"AgentVault gRPC request failed: {error.code().name}") from None
        if len(response.value) > self.config.max_message_bytes:
            raise RuntimeError("AgentVault gRPC response exceeds configured limit")
        try:
            wire = json.loads(response.value)
            status = wire["status"]
            encoded = wire["body"]
            raw = base64.b64decode(encoded + "=" * (-len(encoded) % 4), altchars=b"-_", validate=True)
            value = json.loads(raw)
        except (KeyError, TypeError, ValueError, UnicodeDecodeError):
            raise RuntimeError("invalid AgentVault gRPC response") from None
        if (not isinstance(status, int) or not 100 <= status <= 599
                or not isinstance(value, dict) or len(raw) > self.config.max_message_bytes
                or base64.urlsafe_b64encode(raw).rstrip(b"=").decode("ascii") != encoded):
            raise RuntimeError("invalid or oversized AgentVault gRPC response")
        if not 200 <= status < 300:
            detail = value.get("error", {})
            if not isinstance(detail, dict):
                detail = {}
            raise APIError(status, str(detail.get("code", "TRANSPORT_ERROR")),
                           str(detail.get("message", f"AgentVault returned status {status}")))
        return value

    def close(self) -> None:
        with self._close_lock:
            if self._closed:
                return
            self._channel.close()
            self._closed = True

    def __enter__(self) -> "GrpcVaultTransport":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()
