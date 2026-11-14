from __future__ import annotations

import re
from dataclasses import dataclass

_IDENTIFIER = re.compile(r"^[A-Za-z0-9_.-]{1,128}$")
_PROTOCOL = re.compile(r"^[a-z0-9-]{1,32}$")


class _ProtocolNames:
    @staticmethod
    def validate(protocols: tuple[str, ...]) -> None:
        if (
            not protocols
            or len(set(protocols)) != len(protocols)
            or any(
                not isinstance(protocol, str) or not _PROTOCOL.fullmatch(protocol)
                for protocol in protocols
            )
        ):
            raise ValueError("payment protocols must be unique safe lowercase names")


@dataclass(frozen=True)
class PayerConfig:
    scenario_id: str
    approval_timeout: float = 120.0
    approval_poll_interval: float = 1.0
    delivery_retries: int = 0
    max_challenge_bytes: int = 256 * 1024
    protocols: tuple[str, ...] = ("x402", "mpp")

    def __post_init__(self) -> None:
        if not isinstance(self.scenario_id, str) or not self.scenario_id.strip():
            raise ValueError("scenario_id cannot be blank")
        if (
            self.approval_timeout <= 0
            or self.approval_poll_interval <= 0
            or self.approval_poll_interval > self.approval_timeout
            or not 0 <= self.delivery_retries <= 3
            or self.max_challenge_bytes <= 0
        ):
            raise ValueError(
                "payer middleware retry, timeout, or challenge limits are invalid"
            )
        self._validate_protocols()

    def _validate_protocols(self) -> None:
        _ProtocolNames.validate(self.protocols)


@dataclass(frozen=True)
class PayeeConfig:
    max_body_bytes: int = 1024 * 1024
    protocols: tuple[str, ...] = ("x402", "mpp")

    def __post_init__(self) -> None:
        if self.max_body_bytes <= 0:
            raise ValueError("payee middleware body limit must be positive")
        _ProtocolNames.validate(self.protocols)


@dataclass(frozen=True, repr=False)
class HttpConfig:
    base_url: str
    authguard_bearer: str | None = None
    timeout: float = 30.0
    max_response_bytes: int = 2 * 1024 * 1024

    def __post_init__(self) -> None:
        from urllib.parse import urlsplit

        url = urlsplit(self.base_url)
        if (
            url.scheme not in ("http", "https")
            or not url.hostname
            or url.username
            or url.password
            or url.query
            or url.fragment
            or url.path not in ("", "/")
            or self.timeout <= 0
            or self.max_response_bytes <= 0
            or (
                self.authguard_bearer is not None
                and any(ord(c) < 32 or ord(c) == 127 for c in self.authguard_bearer)
            )
        ):
            raise ValueError("invalid AgentVault HTTP configuration")

    def __repr__(self) -> str:
        return f"HttpConfig(base_url={self.base_url!r}, authguard_bearer=<redacted>)"


@dataclass(frozen=True)
class VaultTransportConfig:
    provider: str
    http: HttpConfig

    def __post_init__(self) -> None:
        if self.provider != "http" or not isinstance(self.http, HttpConfig):
            raise ValueError("transport provider must be http")
