from __future__ import annotations

import ipaddress
import re
from dataclasses import dataclass

_IDENTIFIER = re.compile(r"^[A-Za-z0-9_.-]{1,128}$")


def _loopback(host: str) -> bool:
    if host.lower() == "localhost":
        return True
    try:
        return ipaddress.ip_address(host).is_loopback
    except ValueError:
        return False


@dataclass(frozen=True)
class PayerConfig:
    scenario_id: str
    approval_timeout: float = 120.0
    approval_poll_interval: float = 1.0
    delivery_retries: int = 0
    max_challenge_bytes: int = 256 * 1024

    def __post_init__(self) -> None:
        if not isinstance(self.scenario_id, str) or not self.scenario_id.strip():
            raise ValueError("scenario_id cannot be blank")
        if (
            self.approval_timeout <= 0 or self.approval_poll_interval <= 0
            or self.approval_poll_interval > self.approval_timeout
            or not 0 <= self.delivery_retries <= 3
            or self.max_challenge_bytes <= 0
        ):
            raise ValueError("payer middleware retry, timeout, or challenge limits are invalid")


@dataclass(frozen=True)
class PayeeConfig:
    max_body_bytes: int = 1024 * 1024

    def __post_init__(self) -> None:
        if self.max_body_bytes <= 0:
            raise ValueError("payee middleware body limit must be positive")


@dataclass(frozen=True, repr=False)
class GrpcConfig:
    endpoint: str
    tls: bool = True
    authguard_bearer: str | None = None
    timeout: float = 30.0
    max_message_bytes: int = 2 * 1024 * 1024

    def __post_init__(self) -> None:
        host, separator, port = self.endpoint.rpartition(":")
        if (
            not separator or not host or not port.isdigit() or not 0 < int(port) <= 65535
            or self.timeout <= 0 or self.max_message_bytes <= 0
            or (not self.tls and not _loopback(host.strip("[]")))
            or (
                self.authguard_bearer is not None
                and (not self.authguard_bearer.strip() or any(ord(character) < 0x20 for character in self.authguard_bearer))
            )
        ):
            raise ValueError("invalid or unsafe AgentVault gRPC configuration")

    def __repr__(self) -> str:
        edge = "<redacted>" if self.authguard_bearer else "disabled"
        return f"GrpcConfig(endpoint={self.endpoint!r}, tls={self.tls}, authguard_bearer={edge})"


@dataclass(frozen=True, repr=False)
class MqttConfig:
    host: str
    port: int
    tls: bool
    username: str
    password: str
    connection_prefix: str
    vault_client_id: str
    timeout: float = 30.0
    max_response_bytes: int = 2 * 1024 * 1024

    def __post_init__(self) -> None:
        if (
            not self.host.strip() or not 0 < self.port <= 65535 or self.timeout <= 0
            or self.max_response_bytes <= 0 or not _IDENTIFIER.fullmatch(self.username)
            or not self.password or not _IDENTIFIER.fullmatch(self.connection_prefix)
            or not _IDENTIFIER.fullmatch(self.vault_client_id)
            or (not self.tls and not _loopback(self.host))
        ):
            raise ValueError("invalid or unsafe AgentVault MQTT configuration")

    def __repr__(self) -> str:
        return (
            f"MqttConfig(host={self.host!r}, port={self.port}, tls={self.tls}, "
            f"username=<redacted>, password=<redacted>, connection_prefix={self.connection_prefix!r}, "
            f"vault_client_id={self.vault_client_id!r})"
        )
