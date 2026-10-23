from __future__ import annotations

from abc import ABC, abstractmethod
from dataclasses import dataclass
from threading import RLock
from typing import Callable, Mapping

from ..model import HttpCredential, PaymentChallenge, PaymentCredential, SettledIncomingPayment


@dataclass(frozen=True)
class HttpResponse:
    headers: Mapping[str, str]
    content_type: str | None = None
    body: bytes = b""


@dataclass(frozen=True)
class MerchantEvidence:
    header_name: str
    header_value: str


@dataclass(frozen=True)
class McpMetadata:
    key: str
    value: object


@dataclass(frozen=True)
class McpPaymentRequired:
    result: object | None = None
    error: object | None = None


class PaymentProtocol(ABC):
    @property
    @abstractmethod
    def name(self) -> str: ...

    def credential_header_names(self) -> tuple[str, ...]:
        return ()

    def merchant_evidence_header_names(self) -> tuple[str, ...]:
        return ()

    @abstractmethod
    def credential(self, credential: PaymentCredential) -> HttpCredential: ...

    @abstractmethod
    def merchant_evidence(self, headers: Mapping[str, str]) -> MerchantEvidence | None: ...

    @abstractmethod
    def presented_credential(self, headers: Mapping[str, str], limit: int) -> object | None: ...

    @abstractmethod
    def challenge(self, challenge: PaymentChallenge) -> HttpResponse: ...

    @abstractmethod
    def receipt(self, payment: SettledIncomingPayment) -> HttpResponse: ...

    def mcp_request_metadata_key(self) -> str:
        raise ValueError(f"payment protocol does not support MCP: {self.name}")

    def mcp_root_request_metadata_key(self) -> str:
        raise ValueError(f"payment protocol does not support root JSON-RPC metadata: {self.name}")

    def mcp_receipt_metadata_key(self) -> str:
        raise ValueError(f"payment protocol does not support MCP receipts: {self.name}")

    def mcp_challenge(self, message: object, limit: int) -> HttpResponse | None:
        return None

    def mcp_credential(self, credential: PaymentCredential) -> McpMetadata:
        raise ValueError(f"payment protocol does not support MCP credential delivery: {self.name}")

    def presented_mcp_credential(self, meta: object, limit: int) -> object | None:
        return None

    def mcp_payment_required(self, challenge: PaymentChallenge) -> McpPaymentRequired:
        raise ValueError(f"payment protocol does not support MCP challenges: {self.name}")

    def mcp_payment_rejected(self, challenge: PaymentChallenge, message: str) -> McpPaymentRequired:
        raise ValueError(f"payment protocol does not support MCP verification failures: {self.name}")

    def mcp_receipt(
        self, challenge: PaymentChallenge, payment: SettledIncomingPayment,
    ) -> McpMetadata:
        raise ValueError(f"payment protocol does not support MCP receipts: {self.name}")

    def mcp_merchant_evidence(self, meta: object, limit: int) -> MerchantEvidence | None:
        return None


class ProtocolRegistry:
    def __init__(self, adapters: Mapping[str, PaymentProtocol]) -> None:
        self._adapters = dict(adapters)
        self._priority = tuple(adapters)

    def get(self, name: str) -> PaymentProtocol:
        try:
            return self._adapters[name]
        except KeyError as error:
            raise ValueError(f"payment protocol is not enabled: {name}") from error

    def mcp_challenge(
        self, message: object, limit: int,
    ) -> tuple[PaymentProtocol, HttpResponse] | None:
        for name in self._priority:
            adapter = self.get(name)
            challenge = adapter.mcp_challenge(message, limit)
            if challenge is not None:
                return adapter, challenge
        return None

    def credential_header_names(self) -> tuple[str, ...]:
        names: list[str] = []
        for name in self._priority:
            for candidate in self.get(name).credential_header_names():
                if not any(existing.lower() == candidate.lower() for existing in names):
                    names.append(candidate)
        return tuple(names)

    def merchant_evidence_header_names(self) -> tuple[str, ...]:
        names: list[str] = []
        for name in self._priority:
            for candidate in self.get(name).merchant_evidence_header_names():
                if not any(existing.lower() == candidate.lower() for existing in names):
                    names.append(candidate)
        return tuple(names)

    def presented_credential(
        self, headers: Mapping[str, str], limit: int,
    ) -> tuple[str | None, object | None]:
        selected: str | None = None
        payload: object | None = None
        for name in self._priority:
            value = self.get(name).presented_credential(headers, limit)
            if value is None:
                continue
            if selected is not None:
                raise ValueError("multiple payment protocol credentials were supplied")
            selected, payload = name, value
        return selected, payload

    def merchant_evidence(
        self, headers: Mapping[str, str],
    ) -> tuple[str | None, MerchantEvidence | None]:
        selected: str | None = None
        evidence: MerchantEvidence | None = None
        for name in self._priority:
            value = self.get(name).merchant_evidence(headers)
            if value is None:
                continue
            if selected is not None:
                raise ValueError("multiple payment protocol receipts were supplied")
            selected, evidence = name, value
        return selected, evidence

    def presented_mcp_credential(
        self, meta: object, limit: int,
    ) -> tuple[str | None, object | None]:
        selected: str | None = None
        payload: object | None = None
        for name in self._priority:
            value = self.get(name).presented_mcp_credential(meta, limit)
            if value is None:
                continue
            if selected is not None:
                raise ValueError("multiple MCP payment credentials were supplied")
            selected, payload = name, value
        return selected, payload

    def mcp_merchant_evidence(
        self, meta: object, limit: int,
    ) -> tuple[str | None, MerchantEvidence | None]:
        selected: str | None = None
        evidence: MerchantEvidence | None = None
        for name in self._priority:
            value = self.get(name).mcp_merchant_evidence(meta, limit)
            if value is None:
                continue
            if selected is not None:
                raise ValueError("multiple MCP settlement receipts were supplied")
            selected, evidence = name, value
        return selected, evidence

    def mcp_request_metadata_keys(self) -> tuple[str, ...]:
        keys: list[str] = []
        for name in self._priority:
            try:
                keys.append(self.get(name).mcp_request_metadata_key())
            except ValueError:
                continue
        return tuple(keys)

    def mcp_root_request_metadata_keys(self) -> tuple[str, ...]:
        keys: list[str] = []
        for name in self._priority:
            try:
                keys.append(self.get(name).mcp_root_request_metadata_key())
            except ValueError:
                continue
        return tuple(keys)

    def mcp_receipt_metadata_keys(self) -> tuple[str, ...]:
        keys: list[str] = []
        for name in self._priority:
            try:
                keys.append(self.get(name).mcp_receipt_metadata_key())
            except ValueError:
                continue
        return tuple(keys)


class PaymentProtocolFactory:
    """Creates isolated providers; failed creation is never cached."""

    def __init__(self) -> None:
        from .mpp import MppAdapter
        from .x402 import X402Adapter

        self._lock = RLock()
        self._providers: dict[str, Callable[[], PaymentProtocol]] = {
            "x402": X402Adapter,
            "mpp": MppAdapter,
        }

    def register(self, name: str, provider: Callable[[], PaymentProtocol]) -> None:
        if not name or name != name.lower() or not callable(provider):
            raise ValueError("payment protocol provider registration is invalid")
        with self._lock:
            if name in self._providers:
                raise ValueError(f"payment protocol provider is already registered: {name}")
            self._providers[name] = provider

    def create(self, enabled: tuple[str, ...]) -> ProtocolRegistry:
        if not enabled:
            raise ValueError("payment protocol configuration is empty")
        with self._lock:
            providers = dict(self._providers)
        adapters: dict[str, PaymentProtocol] = {}
        for name in enabled:
            if name in adapters:
                raise ValueError(f"duplicate payment protocol provider: {name}")
            try:
                adapter = providers[name]()
            except KeyError as error:
                raise ValueError(f"payment protocol provider is unavailable: {name}") from error
            if adapter.name != name:
                raise ValueError(f"payment protocol provider identity mismatch: {name}")
            adapters[name] = adapter
        return ProtocolRegistry(adapters)
