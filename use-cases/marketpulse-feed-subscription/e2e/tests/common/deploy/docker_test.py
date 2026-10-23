from __future__ import annotations

import unittest
from json import dumps
from pathlib import Path
from tempfile import TemporaryDirectory
from types import SimpleNamespace
from unittest.mock import Mock, patch

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
    def test_command_progress_operation_omits_sensitive_arguments(self) -> None:
        operation = DockerE2EDeployer._command_operation([
            "sudo", "-n", "-E", "docker", "compose", "--profile", "payer",
            "exec", "agentvault", "client", "--private-key", "do-not-log",
        ])

        self.assertEqual(operation, "docker compose exec profiles=payer target=agentvault")
        self.assertNotIn("do-not-log", operation)

        run_operation = DockerE2EDeployer._command_operation([
            "docker", "compose", "run", "--rm", "-e", "API_TOKEN=do-not-log", "quantstream-rust",
        ])
        self.assertEqual(run_operation, "docker compose run")
        self.assertNotIn("do-not-log", run_operation)

        helm_operation = DockerE2EDeployer._command_operation([
            "sudo", "-n", "env", "KUBECONFIG=/private/path", "helm", "lint", "/tmp/chart",
        ])
        self.assertEqual(helm_operation, "helm lint")
        self.assertNotIn("/private/path", helm_operation)

    def test_component_chart_namespace_is_explicit(self) -> None:
        chart = E2E_ROOT / "deploy" / "anvil" / "helm"

        self.assertEqual(
            DockerE2EDeployer._chart_namespace(chart),
            "e2e-agentvault-payment-system",
        )
        self.assertEqual(DockerE2EDeployer._chart_name(chart), "anvil")

    def test_component_chart_without_namespace_fails_closed(self) -> None:
        with TemporaryDirectory() as directory:
            chart = Path(directory)
            (chart / "Chart.yaml").write_text("apiVersion: v2\nname: unsafe\n", encoding="utf-8")

            with self.assertRaisesRegex(RuntimeError, "does not declare"):
                DockerE2EDeployer._chart_namespace(chart)

    def test_component_chart_rejects_e2e_resource_prefix(self) -> None:
        with TemporaryDirectory() as directory:
            chart = Path(directory)
            (chart / "Chart.yaml").write_text(
                "apiVersion: v2\nname: e2e-anvil\n",
                encoding="utf-8",
            )

            with self.assertRaisesRegex(RuntimeError, "namespace isolation"):
                DockerE2EDeployer._chart_name(chart)

    def test_all_language_services_have_an_isolated_runtime_stage(self) -> None:
        components = (
            *(f"mock-payee-marketpulse-{language}-service" for language in (
                "rust", "golang", "java", "python", "typescript",
            )),
            *(f"mock-payer-quantstream-ingest-service-{language}" for language in (
                "rust", "golang", "java", "python", "typescript",
            )),
        )
        for component in components:
            dockerfile = E2E_ROOT / "deploy" / component / "Dockerfile"
            stages = [
                line.strip()
                for line in dockerfile.read_text(encoding="utf-8").splitlines()
                if line.strip().startswith("FROM ")
            ]
            self.assertEqual(len(stages), 2, f"{component} must have builder and runtime stages")
            self.assertIn(" AS builder", stages[0])
            self.assertIn("_RUNTIME_IMAGE}", stages[1])
            self.assertNotIn("_BUILD_IMAGE}", stages[1])

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


class TempoLifecycleTest(unittest.TestCase):
    @patch("common.deploy.docker.time.sleep")
    @patch("common.deploy.docker.time.time", return_value=1_000)
    def test_faucet_window_requires_a_current_non_genesis_block(
        self,
        _time: Mock,
        _sleep: Mock,
    ) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer._json_rpc_result = Mock(side_effect=[
            {"number": "0x0", "timestamp": "0x3e8"},
            {"number": "0x1", "timestamp": "0x3e8"},
        ])

        deployer._wait_tempo_faucet_window("http://tempo.local", attempts=2)

        self.assertEqual(deployer._json_rpc_result.call_count, 2)
        _sleep.assert_called_once_with(0.5)

    @patch("common.deploy.docker.time.sleep")
    @patch("common.deploy.docker.time.time", return_value=1_000)
    def test_faucet_window_rejects_stale_chain_time(self, _time: Mock, _sleep: Mock) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer._json_rpc_result = Mock(return_value={"number": "0x1", "timestamp": "0x1"})

        with self.assertRaisesRegex(RuntimeError, "faucet nonce window"):
            deployer._wait_tempo_faucet_window("http://tempo.local", attempts=1)

    def test_waits_for_current_official_node_block_before_running_rust_gate(self) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer.context = SimpleNamespace(timeout_seconds=60)
        deployer.compose = ["docker", "compose", "-p", "agentvault-e2e"]
        deployer.environment = {"E2E_TEMPO_RPC_PORT": "28546"}
        deployer.cleanup = Mock()
        deployer._compose = Mock()
        deployer._wait_rpc = Mock()
        deployer._wait_tempo_faucet_window = Mock()
        deployer._target_budget = Mock()
        deployer._run = Mock(return_value="passed")

        self.assertEqual(deployer.mpp_tempo(), "passed")

        deployer._compose.assert_called_once_with(("tempo",), "up", "-d", "tempo")
        deployer._wait_rpc.assert_called_once_with("http://127.0.0.1:28546/", "eth_chainId", "0x539", 90)
        deployer._wait_tempo_faucet_window.assert_called_once_with("http://127.0.0.1:28546/")
        deployer._target_budget.assert_called_once_with()
        deployer._run.assert_called_once()


class SolanaLifecycleTest(unittest.TestCase):
    def test_bootstraps_finalized_spl_fixture_before_running_official_x402_gate(self) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer.context = SimpleNamespace(timeout_seconds=60)
        deployer.environment = {"E2E_SOLANA_RPC_PORT": "28899"}
        deployer.cleanup = Mock()
        deployer._compose = Mock()
        deployer._wait_rpc = Mock()
        deployer._solana_command = Mock()
        deployer._solana_pubkey = Mock(side_effect=["B" * 32, "C" * 32, "D" * 32, "E" * 32])
        deployer._solana_json = Mock(side_effect=[
            {"commandOutput": {"address": "E" * 32}},
            {},
            {},
            {"associatedTokenAddress": "F" * 32},
            {"associatedTokenAddress": "G" * 32},
            {},
        ])
        deployer._wait_solana_token_balance = Mock()
        deployer._solana_rpc_result = Mock(return_value="H" * 44)
        deployer._solana_keypair_json = Mock(side_effect=["[1]", "[2]"])
        deployer._target_budget = Mock()
        deployer._run = Mock(return_value="passed")

        self.assertEqual(deployer.x402_solana(), "passed")

        deployer._compose.assert_called_once_with(("payer",), "up", "-d", "solana")
        deployer._wait_rpc.assert_called_once_with("http://127.0.0.1:28899/", "getHealth", "ok", 120)
        self.assertEqual(
            deployer._wait_solana_token_balance.call_args_list,
            [unittest.mock.call("F" * 32, 1_000_000_000), unittest.mock.call("G" * 32, 0)],
        )
        self.assertEqual(deployer.environment["SOLANA_CHAIN_REFERENCE"], "H" * 32)
        self.assertEqual(deployer.environment["SOLANA_RECIPIENT_ADDRESS"], "C" * 32)
        deployer._target_budget.assert_called_once_with()
        deployer._run.assert_called_once()


class CleanupIsolationTest(unittest.TestCase):
    def test_removes_only_volumes_owned_by_the_compose_project(self) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer.docker = ["docker"]
        deployer._owned_volumes = Mock(side_effect=[["agentvault-e2e-w1_postgres-data"], []])
        deployer._try = Mock(return_value=(0, "removed\n"))

        output = deployer._remove_owned_volumes()

        self.assertEqual(output, "removed\n")
        deployer._try.assert_called_once_with(
            ["docker", "volume", "rm", "--force", "agentvault-e2e-w1_postgres-data"],
            60,
        )

    def test_reports_owned_volume_inventory_failures(self) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer._owned_volumes = Mock(return_value=["<volume inventory unavailable>"])

        with self.assertRaisesRegex(RuntimeError, "could not inventory"):
            deployer._remove_owned_volumes()

    def test_image_inventory_accepts_only_known_local_e2e_images(self) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer.docker = ["docker"]
        deployer._try = Mock(return_value=(0, "\n".join((
            "localhost/quantstream-ingestor-rust-e2e:local",
            "marketpulse-go-e2e:local",
            "example/unrelated-e2e:local",
            "registry.cn-shenzhen.aliyuncs.com/wl4g/agentvault:latest",
        ))))

        self.assertEqual(
            deployer._owned_images(),
            [
                "localhost/quantstream-ingestor-rust-e2e:local",
                "marketpulse-go-e2e:local",
            ],
        )

    def test_removes_only_allowlisted_local_e2e_images(self) -> None:
        deployer = object.__new__(DockerE2EDeployer)
        deployer.docker = ["docker"]
        deployer._owned_images = Mock(side_effect=[["localhost/marketpulse-rust-e2e:local"], []])
        deployer._try = Mock(return_value=(0, "removed\n"))

        output = deployer.cleanup_images()

        self.assertEqual(output, "removed\n")
        deployer._try.assert_called_once_with(
            ["docker", "image", "rm", "localhost/marketpulse-rust-e2e:local"],
            180,
        )


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
