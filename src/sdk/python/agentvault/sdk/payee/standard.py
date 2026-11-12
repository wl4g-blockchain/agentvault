from __future__ import annotations

from ..model import PayeeOperation, ProtectionOutcome
from .payee import PayeeMiddleware


class StandardPayeeMiddleware:
    """Framework-neutral entrypoint for custom Python HTTP/MCP servers."""

    def __init__(self, core: PayeeMiddleware) -> None:
        self.core = core

    def handle(self, operation: PayeeOperation) -> ProtectionOutcome:
        return self.core.protect(operation)
