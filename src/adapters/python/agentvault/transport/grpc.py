from __future__ import annotations

import base64
import json
from typing import Any, Mapping

import grpc
from google.protobuf.wrappers_pb2 import BytesValue

from ..config import GrpcConfig
from .http import APIError

_EXECUTE = "/agentvault.transport.v1.PaymentTransport/Execute"


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
        metadata = ()
        if self.config.authguard_bearer:
            metadata = (("authorization", f"Bearer {self.config.authguard_bearer}"),)
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
            raise APIError(status, str(detail.get("code", "HTTP_ERROR")),
                           str(detail.get("message", f"AgentVault returned HTTP {status}")))
        return value

    def close(self) -> None:
        self._channel.close()

    def __enter__(self) -> "GrpcVaultTransport":
        return self

    def __exit__(self, *_: object) -> None:
        self.close()
