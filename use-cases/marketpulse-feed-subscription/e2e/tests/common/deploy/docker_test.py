from __future__ import annotations

import unittest
from json import dumps
from tempfile import TemporaryDirectory
from pathlib import Path

from common.deploy.docker import (
    BUILD_PROXY_PREFIXES,
    _clean_compose_stdout,
    _inherit_build_proxy_environment,
    _load_matrix_checkpoint,
    _parse_cast_address,
    _parse_cast_uint,
)


class BuildProxyEnvironmentTest(unittest.TestCase):
    def test_inherits_conventional_proxy_without_overriding_component_configuration(self) -> None:
        environment = {
            "HTTPS_PROXY": "http://127.0.0.1:8800",
            "HTTP_PROXY": "http://127.0.0.1:8801",
            "NO_PROXY": "127.0.0.1,localhost",
            "E2E_AGENTVAULT_BUILD_HTTPS_PROXY": "http://explicit.example:9000",
        }

        _inherit_build_proxy_environment(environment)

        self.assertEqual(environment["E2E_AGENTVAULT_BUILD_HTTPS_PROXY"], "http://explicit.example:9000")
        for prefix in BUILD_PROXY_PREFIXES[1:]:
            self.assertEqual(environment[f"{prefix}_HTTPS_PROXY"], "http://127.0.0.1:8800")
        for prefix in BUILD_PROXY_PREFIXES:
            self.assertEqual(environment[f"{prefix}_HTTP_PROXY"], "http://127.0.0.1:8801")
            self.assertEqual(environment[f"{prefix}_NO_PROXY"], "127.0.0.1,localhost")

    def test_cast_parsers_ignore_compose_provider_diagnostics(self) -> None:
        diagnostic = '\x1b[4m>>>> Executing external compose provider "docker-compose". <<<<\n\n\x1b[0m'
        self.assertEqual(
            _parse_cast_address(diagnostic + "0x70997970C51812dc3A010C7d01b50e0d17dc79C8\n"),
            "0x70997970C51812dc3A010C7d01b50e0d17dc79C8",
        )
        self.assertEqual(_parse_cast_uint(diagnostic + "1000000000000 [1e12]\n"), 1_000_000_000_000)

    def test_cast_parsers_fail_closed_when_result_is_missing(self) -> None:
        with self.assertRaises(RuntimeError):
            _parse_cast_address("compose diagnostic only")
        with self.assertRaises(RuntimeError):
            _parse_cast_uint("compose diagnostic only")

    def test_compose_stdout_removes_only_provider_banner(self) -> None:
        output = (
            '\x1b[4m>>>> Executing external compose provider "/usr/bin/docker-compose". '
            'Please refer to the documentation for details. <<<<\n\n\x1b[0m'
            "1|approval-id:pending\n"
        )
        self.assertEqual(_clean_compose_stdout(output), "1|approval-id:pending")


class MatrixCheckpointTest(unittest.TestCase):
    def test_restores_only_complete_evidence_for_the_exact_case(self) -> None:
        reports = {
            "s20_full.md": "full",
            "s21_payer_signing.md": "payer",
            "s22_payee_settlement.md": "payee",
            "s23_balance_sync.md": "sync",
            "s24_gateway_balance.md": "balance",
        }
        with TemporaryDirectory() as directory:
            checkpoint = Path(directory) / "case-01.json"
            checkpoint.write_text(dumps({"case": "expected", "reports": reports}), encoding="utf-8")
            self.assertEqual(_load_matrix_checkpoint(checkpoint, "expected"), reports)
            with self.assertRaisesRegex(RuntimeError, "identity mismatch"):
                _load_matrix_checkpoint(checkpoint, "different")
            checkpoint.write_text(dumps({"case": "expected", "reports": {}}), encoding="utf-8")
            with self.assertRaisesRegex(RuntimeError, "incomplete"):
                _load_matrix_checkpoint(checkpoint, "expected")


if __name__ == "__main__":
    unittest.main()
