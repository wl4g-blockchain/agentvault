from __future__ import annotations

from common.context import VerificationContext
from common.deploy import create_deployer


def verify(context: VerificationContext) -> str:
    return create_deployer(context).full(context.payer_language, context.payee_language)
