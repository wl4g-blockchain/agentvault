"""Manual acceptance retains the stack; a payment is always an explicit action."""
from __future__ import annotations

import json

from common.context import VerificationContext
from deploy.deployer.manual import ManualAcceptanceDeployer


def verify(context: VerificationContext) -> str:
    if context.deployer != "kubernetes":
        raise ValueError("manual acceptance requires --deployer kubernetes")
    deployer = ManualAcceptanceDeployer(context)
    if context.scenario == "manual":
        result = deployer.deploy()
        statement = "Redeployed retained stack. No payer payment was triggered."
    elif context.scenario == "manual-services":
        result = deployer.services()
        statement = "Resumed retained services. Existing chains, Vault, custody, and financial history were preserved."
    elif context.scenario == "manual-payment":
        result = deployer.pay()
        statement = "Executed one real SDK payment; retained Job and chain/ledger evidence."
    elif context.scenario == "manual-matrix":
        result = deployer.acceptance_matrix()
        statement = "Executed 25 cross-language real payments on the retained stack."
    else:
        raise ValueError("unsupported manual action")
    return f"# Manual acceptance\n\n{statement}\n\n```json\n{json.dumps(result, indent=2)}\n```\n"
