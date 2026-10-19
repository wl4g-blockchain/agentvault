from __future__ import annotations

import unittest
from json import dumps
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from unittest.mock import Mock

from common.deploy.docker import (
    AGENTVAULT_CHART,
    AGENTVAULT_CONFIG_TEMPLATE,
    BUILD_PROXY_PREFIXES,
    DockerE2EDeployer,
    _agentvault_build_targets,
    _clean_compose_stdout,
    _ensure_matrix_plan,
    _inherit_build_proxy_environment,
    _load_matrix_checkpoint,
    _matrix_build_profiles,
    _parse_cast_address,
    _parse_cast_uint,
)
from common.project import E2E_ROOT


class BuildProxyEnvironmentTest(unittest.TestCase):
    def test_runtime_config_template_tracks_the_module_layered_chart(self) -> None:
        relative = AGENTVAULT_CONFIG_TEMPLATE.removeprefix("templates/")
        self.assertTrue((AGENTVAULT_CHART / "templates" / relative).is_file())

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


class BuildBoundaryTest(unittest.TestCase):
    def test_source_runtime_is_built_by_default(self) -> None:
        self.assertEqual(_agentvault_build_targets({}), ("agentvault",))

    def test_prebuilt_runtime_is_not_rebuilt(self) -> None:
        environment = {
            "E2E_RUNNER_PREBUILT_AGENTVAULT": "1",
            "E2E_AGENTVAULT_IMAGE": "ghcr.io/example/agentvault:immutable",
        }
        self.assertEqual(_agentvault_build_targets(environment), ())

    def test_prebuilt_runtime_requires_an_explicit_image(self) -> None:
        with self.assertRaisesRegex(RuntimeError, "requires E2E_AGENTVAULT_IMAGE"):
            _agentvault_build_targets({"E2E_RUNNER_PREBUILT_AGENTVAULT": "1"})

    def test_matrix_build_activates_shared_and_language_profiles(self) -> None:
        self.assertEqual(
            _matrix_build_profiles(("rust", "go")),
            (
                "infra", "payee", "apps",
                "payee-rust", "payee-go",
                "ingestor-rust", "ingestor-go",
            ),
        )

    def test_ci_matrix_build_uses_profiles_and_keeps_the_pulled_runtime(self) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer.context = SimpleNamespace(skip_build=False, reuse_build=True)
        deployer.environment = {
            "E2E_RUNNER_PREBUILT_AGENTVAULT": "1",
            "E2E_AGENTVAULT_IMAGE": "ghcr.io/example/agentvault:immutable",
        }
        deployer._target_budget = Mock()
        deployer._compose = Mock()

        deployer._build_matrix_images(("rust", "go"))

        deployer._compose.assert_called_once_with(
            _matrix_build_profiles(("rust", "go")),
            "build",
            "admin-ui",
            "marketpulse-rust",
            "marketpulse-go",
            "quantstream-rust",
            "quantstream-go",
        )

    def test_parallel_rust_builds_use_isolated_locked_caches(self) -> None:
        dockerfiles = {
            "marketpulse-rust": E2E_ROOT / "deploy" / "mock-payee-marketpulse-rust-service" / "Dockerfile",
            "quantstream-rust": E2E_ROOT / "deploy" / "mock-payer-quantstream-ingest-service-rust" / "Dockerfile",
        }
        cache_names = ("cargo-registry", "cargo-git", "target")
        for service, dockerfile in dockerfiles.items():
            contents = dockerfile.read_text(encoding="utf-8")
            for cache_name in cache_names:
                self.assertIn(
                    f"id={service}-{cache_name},sharing=locked",
                    contents,
                    f"{service} must isolate its writable {cache_name} cache",
                )


class ApprovalContainerDiagnosticsTest(unittest.TestCase):
    def test_reports_logs_when_ingestor_exits_before_approval(self) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer.docker = ["docker"]
        deployer._try = Mock(side_effect=[(0, "exited|17\n"), (0, "authorization failed\n")])

        evidence = deployer._stopped_container_evidence("approval-ingestor")

        self.assertIn("state='exited|17'", evidence)
        self.assertIn("authorization failed", evidence)
        self.assertEqual(deployer._try.call_count, 2)

    def test_ignores_running_ingestor(self) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer.docker = ["docker"]
        deployer._try = Mock(return_value=(0, "running|0\n"))

        self.assertIsNone(deployer._stopped_container_evidence("approval-ingestor"))
        deployer._try.assert_called_once()


class FacilitatorRuntimeEvidenceTest(unittest.TestCase):
    def test_canonicalizes_supported_exact_chain_capabilities(self) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer.environment = {"E2E_FACILITATOR_PORT": "38085"}
        deployer._http = Mock(return_value=(200, {}, '{"schemes":["exact"],"chains":["eip155:31337"]}'))

        capabilities = deployer._facilitator_capabilities()

        self.assertEqual(capabilities, '{"chains": ["eip155:31337"], "schemes": ["exact"]}')

    def test_rejects_missing_required_capability(self) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer.environment = {"E2E_FACILITATOR_PORT": "38085"}
        deployer._http = Mock(return_value=(200, {}, '{"chains":["eip155:31337"]}'))

        with self.assertRaisesRegex(RuntimeError, "does not advertise"):
            deployer._facilitator_capabilities()

    def test_rejects_invalid_supported_json(self) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer.environment = {"E2E_FACILITATOR_PORT": "38085"}
        deployer._http = Mock(return_value=(200, {}, "not-json"))

        with self.assertRaisesRegex(RuntimeError, "invalid /supported JSON"):
            deployer._facilitator_capabilities()


class MatrixCheckpointTest(unittest.TestCase):
    def test_matrix_plan_prevents_cross_scope_resume(self) -> None:
        with TemporaryDirectory() as directory:
            plan = Path(directory) / "matrix-plan.json"
            payload = _ensure_matrix_plan(plan, "simple")

            self.assertEqual(payload["scope"], "simple")
            self.assertEqual(len(payload["cases"]), 8)
            self.assertEqual(_ensure_matrix_plan(plan, "simple"), payload)
            with self.assertRaisesRegex(RuntimeError, "immutable resume plan"):
                _ensure_matrix_plan(plan, "full")

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
