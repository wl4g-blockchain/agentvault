from __future__ import annotations

from concurrent.futures import Future
from typing import Any, Mapping, Protocol


class VaultTransport(Protocol):
    def request(
        self,
        method: str,
        path: str,
        body: Mapping[str, Any] | None = None,
    ) -> dict[str, Any]: ...


class AsyncVaultTransport(VaultTransport, Protocol):
    def request_async(
        self,
        method: str,
        path: str,
        body: Mapping[str, Any] | None = None,
    ) -> Future[dict[str, Any]]: ...


class APIError(RuntimeError):
    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(f"AgentVault API error {status} ({code}): {message}")
        self.status, self.code = status, code
