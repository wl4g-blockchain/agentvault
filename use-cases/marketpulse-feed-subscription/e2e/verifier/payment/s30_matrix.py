from __future__ import annotations

from common.context import VerificationContext
from common.deploy import create_deployer


def verify(context: VerificationContext) -> dict[str, str]:
    reports = create_deployer(context).matrix()
    evidence = reports.get("s30_matrix.md", "")
    expected = {
        f"payer={payer} payee={payee} transport={transport}"
        for transport in ("mqtt", "grpc")
        for payer in ("rust", "go", "python", "typescript", "java")
        for payee in ("rust", "go", "python", "typescript", "java")
    }
    missing = sorted(case for case in expected if case not in evidence)
    if "passed 50/50 isolated cases" not in evidence or missing:
        raise RuntimeError(f"matrix evidence is incomplete; missing={missing}")
    for report in ("s21_payer_signing.md", "s22_payee_settlement.md",
                   "s23_balance_sync.md", "s24_gateway_balance.md"):
        detail = reports.get(report, "")
        missing_details = sorted(case for case in expected if f"[{case}]" not in detail)
        if missing_details:
            raise RuntimeError(f"matrix did not preserve all 50 {report} verifications: {missing_details}")
    return reports
