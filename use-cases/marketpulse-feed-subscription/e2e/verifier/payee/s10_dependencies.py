from __future__ import annotations

from common.context import VerificationContext
from deploy.deployer import create_deployer


def verify(context: VerificationContext) -> str:
    return create_deployer(context).smoke("payee")
