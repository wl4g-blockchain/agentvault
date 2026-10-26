from __future__ import annotations

from common.context import VerificationContext

from .base import E2EDeployer
from .docker import DockerE2EDeployer


def create_deployer(context: VerificationContext) -> E2EDeployer:
    if context.deployer == DockerE2EDeployer.backend:
        return DockerE2EDeployer(context)
    if context.deployer == "kubernetes":
        from .kubernetes import KubernetesE2EDeployer

        return KubernetesE2EDeployer(context)
    raise ValueError(f"unsupported E2E deployer: {context.deployer}")


__all__ = ["E2EDeployer", "create_deployer"]
