from __future__ import annotations

import json
import urllib.error
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any, Mapping, Protocol


class VaultTransport(Protocol):
    def request(
        self,
        method: str,
        path: str,
        body: Mapping[str, Any] | None = None,
        idempotency_key: str | None = None,
    ) -> dict[str, Any]: ...


class APIError(RuntimeError):
    def __init__(self, status: int, code: str, message: str) -> None:
        super().__init__(f"AgentVault API error {status} ({code}): {message}")
        self.status, self.code = status, code


class _NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, req: Any, fp: Any, code: int, msg: str, headers: Any, newurl: str) -> None:
        return None


@dataclass(frozen=True, repr=False)
class HttpVaultTransport:
    base_url: str
    authguard_bearer: str | None = None
    timeout: float = 30.0
    max_response_bytes: int = 2 * 1024 * 1024

    def __post_init__(self) -> None:
        parsed = urllib.parse.urlsplit(self.base_url)
        if parsed.scheme not in ("http", "https") or not parsed.netloc or parsed.username or parsed.query or parsed.fragment:
            raise ValueError("unsafe AgentVault base URL")
        if (self.authguard_bearer is not None
                and (not self.authguard_bearer.strip()
                     or any(ord(character) < 0x20 for character in self.authguard_bearer))):
            raise ValueError("invalid AuthGuard bearer credential")
        if self.timeout <= 0 or self.max_response_bytes <= 0:
            raise ValueError("invalid transport limit")

    def __repr__(self) -> str:
        parsed = urllib.parse.urlsplit(self.base_url)
        edge = "<redacted>" if self.authguard_bearer else "disabled"
        return f"HttpVaultTransport(origin={parsed.scheme + '://' + parsed.netloc!r}, authguard_bearer={edge})"

    def request(self, method: str, path: str, body: Mapping[str, Any] | None = None,
                idempotency_key: str | None = None) -> dict[str, Any]:
        if not path.startswith("/") or path.startswith("//") or "\\" in path:
            raise ValueError("AgentVault path must be origin-relative")
        base = urllib.parse.urlsplit(self.base_url)
        target = urllib.parse.urlunsplit((base.scheme, base.netloc, path, "", ""))
        encoded = None if body is None else json.dumps(body, separators=(",", ":"), ensure_ascii=False).encode()
        headers = {"Accept": "application/json"}
        if self.authguard_bearer:
            headers["Authorization"] = f"Bearer {self.authguard_bearer}"
        if encoded is not None:
            headers["Content-Type"] = "application/json"
        if idempotency_key:
            headers["Idempotency-Key"] = idempotency_key
        request = urllib.request.Request(target, data=encoded, headers=headers, method=method)
        try:
            response = urllib.request.build_opener(_NoRedirect).open(request, timeout=self.timeout)
        except urllib.error.HTTPError as error:
            with error:
                raw = error.read(self.max_response_bytes + 1)
            if len(raw) > self.max_response_bytes:
                raise RuntimeError("AgentVault response exceeds configured limit") from None
            try:
                envelope = json.loads(raw)
                detail = envelope.get("error", {})
            except (ValueError, AttributeError):
                detail = {}
            raise APIError(error.code, detail.get("code", "HTTP_ERROR"),
                           detail.get("message", f"AgentVault returned HTTP {error.code}")) from None
        with response:
            raw = response.read(self.max_response_bytes + 1)
        if len(raw) > self.max_response_bytes:
            raise RuntimeError("AgentVault response exceeds configured limit")
        value = json.loads(raw)
        if not isinstance(value, dict):
            raise RuntimeError("AgentVault response must be a JSON object")
        return value
