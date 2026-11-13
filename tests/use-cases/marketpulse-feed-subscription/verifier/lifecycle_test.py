import importlib
import tempfile
import unittest
from pathlib import Path
from types import SimpleNamespace
from unittest.mock import Mock, patch

from runner import SCENARIOS, verifier
from util.constant import E2EOtherDefinitions as Project
from util.report import E2EReportHelper
from verifier.purchase.s30_matrix import E2ES30MatrixVerifier


class LifecycleTest(unittest.TestCase):
    def test_all_registered_scenarios_have_three_phases(self):
        for scenario in SCENARIOS:
            with self.subTest(scenario=scenario.name):
                self.assertTrue(callable(verifier(scenario)))

    def test_phase_order_and_session_release(self):
        for failure in (None, "deploy", "init", "verify"):
            with self.subTest(failure=failure):
                events = []
                context = SimpleNamespace(state={"session": Mock()})
                session = context.state["session"]

                def phase(name):
                    def call(context):
                        events.append(name)
                        if name == failure:
                            raise RuntimeError(name)
                        return "evidence"

                    return call

                candidate = SimpleNamespace(
                    **{name: phase(name) for name in ("deploy", "init", "verify")}
                )
                module = SimpleNamespace(E2ES00InfrastructureVerifier=candidate)
                with patch("runner.importlib.import_module", return_value=module):
                    if failure:
                        with self.assertRaises(RuntimeError):
                            verifier(SCENARIOS[0])(context)
                    else:
                        self.assertEqual(verifier(SCENARIOS[0])(context), "evidence")
                expected = ["deploy", "init", "verify"]
                if failure:
                    expected = expected[: expected.index(failure) + 1]
                self.assertEqual(events, expected)
                session.close.assert_called_once()

    def test_all_verifier_modules_import(self):
        for path in (Project.E2E_ROOT / "verifier").glob("*/s*.py"):
            with self.subTest(path=path.name):
                importlib.import_module(
                    ".".join(path.relative_to(Project.E2E_ROOT).with_suffix("").parts)
                )

    @patch.object(E2ES30MatrixVerifier.SdkMatrixJournal, "_verify_financial_evidence")
    def test_matrix_scope_and_resume_integrity(self, recheck):
        self.assertEqual(len(Project.matrix_cases("simple")), 8)
        self.assertEqual(len(Project.matrix_cases("full")), 50)
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            journal = E2ES30MatrixVerifier.SdkMatrixJournal(root, "simple", "docker")
            name = Project.matrix_cases("simple")[0].name
            outputs = {name: "evidence" for name in journal.REPORTS}
            self._seal_case(root, Project.matrix_cases("simple")[0])
            journal.save(1, name, outputs)
            self.assertEqual(journal.load(1, name), outputs)
            with self.assertRaises(RuntimeError):
                E2ES30MatrixVerifier.SdkMatrixJournal(root, "full", "docker")
            with self.assertRaises(RuntimeError):
                E2ES30MatrixVerifier.SdkMatrixJournal(root, "simple", "kubernetes")
            with self.assertRaises(ValueError):
                journal.load(2, name)
            with self.assertRaises(RuntimeError):
                journal.save(1, name, {})

    def test_full_init_checks_payer_and_merchant_wallets(self):
        from verifier.purchase.s20_purchase import E2ES20PurchaseVerifier

        runtime = Mock()
        runtime._sql.return_value = "11\n"
        context = SimpleNamespace(payer_language="rust", payee_language="rust")
        with patch(
            "verifier.purchase.s20_purchase.create_deployer", return_value=runtime
        ):
            E2ES20PurchaseVerifier.init(context)
            runtime._sql.assert_called_with(
                "rust", "rust", "SELECT count(*) FROM pay_wallet"
            )
            runtime._sql.return_value = "10\n"
            with self.assertRaises(RuntimeError):
                E2ES20PurchaseVerifier.init(context)

    def test_structure_rejects_unowned_functions_in_module_branches(self):
        import ast

        from verifier.infra.s01_structure import E2ES01StructureVerifier

        check = E2ES01StructureVerifier.StructureVerifier._require_class_owned_functions
        for source in ("def helper(): pass", "if True:\n    def helper(): pass"):
            with self.subTest(source=source), self.assertRaises(RuntimeError):
                check(ast.parse(source), Path("example.py"))
        check(
            ast.parse(
                "class Owner:\n    def operation(self):\n        def closure(): pass"
            ),
            Path("example.py"),
        )

    def test_dependencies_run_only_the_requested_lifecycle_phase(self):
        for role in ("payer", "payee"):
            module = importlib.import_module("verifier.infra.s00_infrastructure")
            runtime = Mock()
            with patch.object(module, "create_deployer", return_value=runtime):
                for phase, operation in (
                    ("deploy", "deploy_dependencies"),
                    ("init", "initialize_dependencies"),
                    ("verify", "verify_dependencies"),
                ):
                    with self.subTest(role=role, phase=phase):
                        runtime.reset_mock()
                        getattr(module.E2ES00InfrastructureVerifier, phase)(
                            SimpleNamespace(scenario=role)
                        )
                        self.assertEqual(
                            [call[0] for call in runtime.mock_calls], [operation]
                        )
                        getattr(runtime, operation).assert_called_once_with(role)

    def test_matrix_full_facade_preserves_three_stage_order(self):
        from verifier.purchase.s20_purchase import E2ES20PurchaseVerifier

        for implementation in (
            E2ES20PurchaseVerifier.DockerRuntime,
            E2ES20PurchaseVerifier.KubernetesRuntime,
        ):
            with self.subTest(backend=implementation.backend):
                runtime = object.__new__(implementation)
                events = []
                runtime.prepare_full = Mock(
                    side_effect=lambda *args, **kwargs: events.append("deploy")
                )
                runtime.initialize_full = Mock(
                    side_effect=lambda *args: events.append("init")
                )
                runtime.verify_full = Mock(
                    side_effect=lambda *args: (
                        events.append("verify") or {"proof": "passed"}
                    )
                )
                self.assertEqual(runtime.full("rust", "go"), {"proof": "passed"})
                self.assertEqual(events, ["deploy", "init", "verify"])

    def test_facilitator_readiness_waits_until_chain_initialization(self):
        from contextlib import nullcontext

        from verifier.purchase.s20_purchase import E2ES20PurchaseVerifier

        runtime = object.__new__(E2ES20PurchaseVerifier.KubernetesRuntime)
        runtime._component_chart = Mock(return_value=Path("component/helm"))
        runtime._helm_install = Mock()
        runtime._install_dependencies("payee", include_solana=False)
        facilitator = runtime._helm_install.call_args_list[-1]
        self.assertEqual(facilitator.args[0], "x402-facilitator")
        self.assertIs(facilitator.kwargs["wait"], False)
        events = []
        runtime._helm_install = Mock(
            side_effect=lambda *args, **kwargs: events.append("contracts")
        )
        runtime._runtime_forwards = Mock(return_value=nullcontext())
        runtime._bootstrap_emqx = Mock(side_effect=lambda: events.append("acl"))
        runtime._kubectl = Mock(
            side_effect=lambda *args, **kwargs: events.append(args[1])
        )
        runtime._initialize_base()
        self.assertEqual(events, ["contracts", "acl", "restart", "status"])

    @patch.object(E2ES30MatrixVerifier.SdkMatrixJournal, "_verify_financial_evidence")
    def test_matrix_deployment_preserves_fully_restored_evidence(self, recheck):
        from verifier.purchase.s20_purchase import E2ES20PurchaseVerifier

        for restored in (False, True):
            with (
                self.subTest(restored=restored),
                tempfile.TemporaryDirectory() as directory,
            ):
                root = Path(directory)
                runtime = object.__new__(E2ES20PurchaseVerifier.DockerRuntime)
                runtime.context = SimpleNamespace(
                    report_root=root, e2e_sdk_scope="simple", state={}
                )
                runtime.cleanup = Mock()
                runtime._build_matrix_images = Mock()
                if restored:
                    journal = E2ES30MatrixVerifier.SdkMatrixJournal(
                        root / "payment", "simple", "docker"
                    )
                    for number, case in enumerate(Project.matrix_cases("simple"), 1):
                        self._seal_case(root / "payment", case)
                        journal.save(
                            number,
                            case.name,
                            {name: "evidence" for name in journal.REPORTS},
                        )
                runtime.prepare_matrix()
                self.assertTrue(runtime.context.state["matrix_prepared"])
                self.assertEqual(runtime.cleanup.call_count, 0 if restored else 1)
                self.assertEqual(
                    runtime._build_matrix_images.call_count, 0 if restored else 1
                )

    def test_manual_phases_initialize_without_triggering_payments(self):
        from verifier.acceptance.s29_retained_purchase import (
            E2ES29RetainedPurchaseVerifier as Verifier,
        )

        for scenario in (
            "manual",
            "manual-services",
            "manual-payment",
            "manual-matrix",
        ):
            with self.subTest(scenario=scenario):
                runtime = Mock()
                runtime.initialize.return_value = {"ready": True}
                runtime.services.return_value = {"ready": True}
                runtime.pay.return_value = {"paid": True}
                runtime.acceptance_matrix.return_value = [{"paid": True}]
                context = SimpleNamespace(
                    deployer="kubernetes", scenario=scenario, state={}
                )
                with patch.object(
                    Verifier, "ManualAcceptanceDeployer", return_value=runtime
                ):
                    Verifier.deploy(context)
                    runtime.initialize.assert_not_called()
                    runtime.pay.assert_not_called()
                    Verifier.init(context)
                    runtime.pay.assert_not_called()
                    Verifier.verify(context)
                self.assertEqual(runtime.deploy.call_count, int(scenario == "manual"))
                self.assertEqual(
                    runtime.initialize.call_count, int(scenario == "manual")
                )
                self.assertEqual(
                    runtime.pay.call_count, int(scenario == "manual-payment")
                )
                self.assertEqual(
                    runtime.acceptance_matrix.call_count,
                    int(scenario == "manual-matrix"),
                )

    @staticmethod
    def _seal_case(root, case):
        directory = root / "full-flow" / f"{case.payer}-{case.payee}-{case.transport}"
        directory.mkdir(parents=True, exist_ok=True)
        (directory / "raw.json").write_text('{"settled":true}')
        E2EReportHelper.seal(
            directory,
            {"source": E2EReportHelper.source_identity(Project.REPOSITORY_ROOT)},
        )

    def test_checkpoint_rejects_changed_raw_evidence(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            case = Project.matrix_cases("simple")[0]
            journal = E2ES30MatrixVerifier.SdkMatrixJournal(root, "simple", "docker")
            self._seal_case(root, case)
            journal.save(1, case.name, {name: "evidence" for name in journal.REPORTS})
            (
                root
                / "full-flow"
                / f"{case.payer}-{case.payee}-{case.transport}"
                / "raw.json"
            ).write_text('{"settled":false}')
            with self.assertRaisesRegex(RuntimeError, "evidence content changed"):
                journal.load(1, case.name)

    def test_dependencies_precede_requested_observation_and_aliases_run_once(self):
        from runner import selected_scenarios

        self.assertEqual(
            [s.name for s in selected_scenarios(["metrics,x402-evm,full,logs"])],
            ["full", "metrics", "logs"],
        )
        self.assertEqual(
            [s.name for s in selected_scenarios(["identity"])], ["full", "identity"]
        )

    def test_kubernetes_does_not_derive_from_docker_runtime(self):
        from verifier.purchase.s20_purchase import E2ES20PurchaseVerifier as Purchase

        self.assertNotIn(Purchase.DockerRuntime, Purchase.KubernetesRuntime.__mro__)
        for cls in (Purchase.DockerRuntime, Purchase.KubernetesRuntime):
            self.assertTrue(callable(cls._facilitator_capabilities))

    def test_docker_workers_isolate_every_host_port_including_grpc(self):
        from verifier.infra.s00_infrastructure import DEFAULTS
        from verifier.purchase.s20_purchase import E2ES20PurchaseVerifier

        ports = [name for name in DEFAULTS if name.endswith("_PORT")]
        for overrides in ({}, {"E2E_AGENTVAULT_GRPC_PORT": "46000"}):
            with (
                self.subTest(overrides=overrides),
                patch.dict("os.environ", overrides, clear=True),
                patch("util.docker.E2EDockerDeployer.detect", return_value=["docker"]),
                patch(
                    "util.docker.E2EDockerDeployer.compose_command",
                    return_value=["docker", "compose"],
                ),
            ):
                allocated = set()
                for slot in (None, 0, 1):
                    context = SimpleNamespace(worker_slot=slot, environment=lambda: {})
                    runtime = E2ES20PurchaseVerifier.DockerRuntime(context)
                    bindings = {int(runtime.environment[name]) for name in ports}
                    self.assertEqual(len(bindings), len(ports))
                    self.assertFalse(allocated & bindings, allocated & bindings)
                    allocated.update(bindings)
