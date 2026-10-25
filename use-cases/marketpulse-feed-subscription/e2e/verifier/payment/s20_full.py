from __future__ import annotations

from common.context import VerificationContext
from deploy.deployer import create_deployer


def verify(context: VerificationContext) -> dict[str, str]:
    return create_deployer(context).full(context.payer_language, context.payee_language)
