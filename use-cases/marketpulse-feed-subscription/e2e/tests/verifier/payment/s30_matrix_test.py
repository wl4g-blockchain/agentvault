from __future__ import annotations

import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from common.context import VerificationContext
from common.sdk_matrix import TRANSPORTS, matrix_cases, matrix_languages
from verifier.payment.s30_matrix import verify


def _context(scope: str) -> VerificationContext:
    return VerificationContext(
        run_id="run",
        report_root=Path("reports/run"),
        deploy_root=Path("deploy"),
        scenario="matrix",
        round_number=1,
        payer_language="rust",
        payee_language="rust",
        transport="mqtt",
        deployer="docker",
        timeout_seconds=60,
        skip_build=True,
        reuse_build=False,
        e2e_sdk_scope=scope,
    )


def _reports(scope: str) -> dict[str, str]:
    cases = matrix_cases(scope)
    names = [case.name for case in cases]
    details = "\n\n".join(f"[{name}]\nverified" for name in names)
    return {
        "s30_matrix.md": (
            f"scope={scope} languages={','.join(matrix_languages(scope))} "
            f"transports={','.join(TRANSPORTS)}\n"
            f"passed {len(cases)}/{len(cases)} isolated cases\n\n"
            + "\n".join(names)
        ),
        "s21_payer_signing.md": details,
        "s22_payee_settlement.md": details,
        "s23_balance_sync.md": details,
        "s24_gateway_balance.md": details,
    }


class MatrixVerifierTest(unittest.TestCase):
    def test_simple_scope_accepts_exactly_eight_cases(self) -> None:
        deployer = Mock()
        deployer.matrix.return_value = _reports("simple")
        with patch("verifier.payment.s30_matrix.create_deployer", return_value=deployer):
            reports = verify(_context("simple"))

        self.assertIn("passed 8/8 isolated cases", reports["s30_matrix.md"])

    def test_simple_scope_rejects_full_evidence(self) -> None:
        deployer = Mock()
        deployer.matrix.return_value = _reports("full")
        with patch("verifier.payment.s30_matrix.create_deployer", return_value=deployer):
            with self.assertRaisesRegex(RuntimeError, "omitted scope=simple"):
                verify(_context("simple"))


if __name__ == "__main__":
    unittest.main()
