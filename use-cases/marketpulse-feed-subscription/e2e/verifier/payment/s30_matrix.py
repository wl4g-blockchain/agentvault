from __future__ import annotations

import re

from common.context import VerificationContext
from common.deploy import create_deployer
from common.sdk_matrix import matrix_cases


def verify(context: VerificationContext) -> dict[str, str]:
    reports = create_deployer(context).matrix()
    evidence = reports.get("s30_matrix.md", "")
    cases = matrix_cases(context.e2e_sdk_scope)
    expected = {case.name for case in cases}
    total = len(cases)
    reported = {line for line in evidence.splitlines() if line.startswith("payer=")}
    if f"scope={context.e2e_sdk_scope} " not in evidence:
        raise RuntimeError(f"matrix evidence omitted scope={context.e2e_sdk_scope}")
    if f"passed {total}/{total} isolated cases" not in evidence or reported != expected:
        raise RuntimeError(
            f"matrix evidence does not match scope={context.e2e_sdk_scope}; "
            f"missing={sorted(expected - reported)}, extra={sorted(reported - expected)}"
        )
    for report in ("s21_payer_signing.md", "s22_payee_settlement.md",
                   "s23_balance_sync.md", "s24_gateway_balance.md"):
        detail = reports.get(report, "")
        detailed_cases = re.findall(r"^\[(payer=.*)\]$", detail, re.MULTILINE)
        detailed_set = set(detailed_cases)
        if len(detailed_cases) != total or detailed_set != expected:
            raise RuntimeError(
                f"matrix scope={context.e2e_sdk_scope} did not preserve all "
                f"{total} {report} verifications: missing={sorted(expected - detailed_set)}, "
                f"extra={sorted(detailed_set - expected)}"
            )
    return reports
