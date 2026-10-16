from __future__ import annotations

from abc import ABC, abstractmethod

from common.context import VerificationContext


class E2EDeployer(ABC):
    """Backend-neutral lifecycle used by every E2E verifier."""

    backend = ""

    def __init__(self, context: VerificationContext) -> None:
        self.context = context

    @abstractmethod
    def render(self) -> str:
        """Validate and render the complete deployment."""

    @abstractmethod
    def smoke(self, role: str) -> str:
        """Start and verify dependencies for one payment role."""

    @abstractmethod
    def full(self, payer_language: str, payee_language: str) -> str:
        """Run one payer/payee language pair through the complete payment flow."""

    @abstractmethod
    def matrix(self) -> str:
        """Run every supported language through the payment flow."""

    @abstractmethod
    def cleanup(self) -> str:
        """Remove only resources owned by this E2E deployment."""
