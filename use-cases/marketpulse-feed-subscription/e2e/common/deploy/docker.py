from __future__ import annotations

from common.process import run

from .base import E2EDeployer


class DockerE2EDeployer(E2EDeployer):
    """Docker Compose lifecycle for the isolated AgentVault E2E project."""

    backend = "docker"

    def _execute(self, *arguments: str) -> str:
        return run(
            ["bash", "e2e.sh", *arguments],
            self.context.deploy_root,
            self.context.environment(),
            self.context.timeout_seconds,
        )

    def render(self) -> str:
        return self._execute("config")

    def smoke(self, role: str) -> str:
        if role not in {"payer", "payee"}:
            raise ValueError(f"unsupported payment role: {role}")
        return self._execute("smoke", role)

    def full(self, payer_language: str, payee_language: str) -> str:
        return self._execute("full", payer_language, payee_language)

    def matrix(self) -> str:
        return self._execute("matrix")

    def cleanup(self) -> str:
        return self._execute("down")
