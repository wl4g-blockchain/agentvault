from __future__ import annotations

from contextlib import nullcontext
import os
from pathlib import Path
import subprocess
from types import SimpleNamespace
import unittest
from unittest.mock import Mock, patch

from common.deploy import create_deployer
from common.deploy.kubernetes_deployer import (
    AGENTVAULT_NAMESPACE,
    KUBERNETES_AGENTVAULT_RUNTIME_IMAGE,
    LOCAL_IMAGES,
    PAYEE_NAMESPACE,
    PAYMENT_NAMESPACE,
    KubernetesE2EDeployer,
)


class KubernetesE2EDeployerTest(unittest.TestCase):
    def deployer(self) -> KubernetesE2EDeployer:
        deployer = object.__new__(KubernetesE2EDeployer)
        deployer.context = SimpleNamespace(
            transport="grpc",
            timeout_seconds=60,
            deploy_root=Path("/tmp/deploy"),
        )
        deployer.environment = {
            "E2E_TEMPO_RPC_PORT": "28546",
            "E2E_QUANTSTREAM_UI_TOKEN": "test-token",
        }
        return deployer

    @patch("common.deploy.kubernetes_deployer.execute")
    @patch("common.deploy.kubernetes_deployer.shutil.which")
    def test_uses_minimal_agentvault_runtime_without_overriding_operator_choice(
        self,
        which: Mock,
        execute: Mock,
    ) -> None:
        context = Mock(worker_slot=None)
        context.environment.return_value = {}
        which.side_effect = lambda executable: f"/usr/bin/{executable}"
        execute.return_value = subprocess.CompletedProcess([], 0, "")
        kubeconfig = Path(__file__)

        with patch.dict(os.environ, {"KUBECONFIG": str(kubeconfig)}, clear=False):
            os.environ.pop("E2E_AGENTVAULT_RUNTIME_IMAGE", None)
            deployer = KubernetesE2EDeployer(context)
        self.assertEqual(
            deployer.environment["E2E_AGENTVAULT_RUNTIME_IMAGE"],
            KUBERNETES_AGENTVAULT_RUNTIME_IMAGE,
        )

        with patch.dict(
            os.environ,
            {
                "KUBECONFIG": str(kubeconfig),
                "E2E_AGENTVAULT_RUNTIME_IMAGE": "registry.example/runtime:pinned",
            },
        ):
            overridden = KubernetesE2EDeployer(context)
        self.assertEqual(
            overridden.environment["E2E_AGENTVAULT_RUNTIME_IMAGE"],
            "registry.example/runtime:pinned",
        )

    @patch("common.deploy.kubernetes_deployer.KubernetesE2EDeployer")
    def test_factory_selects_kubernetes_backend(self, deployer_type: Mock) -> None:
        context = SimpleNamespace(deployer="kubernetes")

        result = create_deployer(context)

        deployer_type.assert_called_once_with(context)
        self.assertIs(result, deployer_type.return_value)

    def test_protocol_and_matrix_scenarios_do_not_fall_back_to_docker(self) -> None:
        for method in ("matrix", "mpp_tempo", "x402_solana"):
            self.assertIn(method, KubernetesE2EDeployer.__dict__)

    def test_payer_job_uses_cluster_dns_and_selected_transport(self) -> None:
        deployer = self.deployer()

        values = deployer._payer_values("rust", "go")

        self.assertEqual(values["env.AGENTVAULT_TRANSPORT"], "grpc")
        self.assertEqual(
            values["env.MARKETPULSE_URL"],
            f"http://marketpulse-golang.{PAYEE_NAMESPACE}.svc.cluster.local:8081/mcp",
        )
        self.assertEqual(
            values["env.AGENTVAULT_GRPC_ENDPOINT"],
            f"agentvault.{AGENTVAULT_NAMESPACE}.svc.cluster.local:50051",
        )

    def test_solana_fixture_commands_execute_inside_validator_pod(self) -> None:
        deployer = self.deployer()
        deployer._kubectl = Mock(return_value="fixture-output\n")  # type: ignore[method-assign]

        output = deployer._solana_command("solana-keygen", "pubkey", "/tmp/key.json")

        self.assertEqual(output, "fixture-output")
        arguments = deployer._kubectl.call_args.args  # type: ignore[attr-defined]
        self.assertEqual(arguments[:4], ("exec", "--namespace", PAYMENT_NAMESPACE, "deployment/solana"))
        self.assertIn("/opt/solana/solana-release/bin/solana-keygen", arguments)

    @patch("common.deploy.kubernetes_deployer.subprocess.run")
    def test_secret_payload_is_passed_over_stdin_not_command_arguments(self, run: Mock) -> None:
        deployer = self.deployer()
        deployer.kubeconfig = Path("/tmp/kubeconfig")
        deployer._kubectl_executable = "/usr/bin/kubectl"
        deployer._privileged_command = Mock(  # type: ignore[method-assign]
            return_value=["kubectl", "apply", "-f", "-"]
        )
        run.return_value = subprocess.CompletedProcess([], 0, "applied")

        deployer._apply_secret("e2e-agentvault-system", "test-secret", {"token": "private"})

        command = run.call_args.args[0]
        self.assertNotIn("private", " ".join(command))
        self.assertIn('"token":"private"', run.call_args.kwargs["input"])

    @patch("common.deploy.kubernetes_deployer.execute")
    def test_cleanup_images_removes_only_allowlisted_e2e_references(self, execute: Mock) -> None:
        deployer = self.deployer()
        deployer.docker = ["docker"]
        deployer._cluster_images = Mock(  # type: ignore[method-assign]
            return_value={*LOCAL_IMAGES.values(), "aliyuncs.com/formal/service:stable"}
        )
        execute.return_value = subprocess.CompletedProcess([], 0, "removed")

        deployer.cleanup_images()

        commands = [call.args[0] for call in execute.call_args_list]
        rendered = "\n".join(" ".join(command) for command in commands)
        self.assertNotIn("aliyuncs.com/formal/service:stable", rendered)
        for reference in LOCAL_IMAGES.values():
            self.assertIn(reference, rendered)

    def test_mpp_tempo_uses_helm_localnet_and_host_port_forward(self) -> None:
        deployer = self.deployer()
        deployer.cleanup = Mock()  # type: ignore[method-assign]
        deployer._apply_namespaces = Mock()  # type: ignore[method-assign]
        deployer._component_chart = Mock(return_value=Path("/charts/tempo"))  # type: ignore[method-assign]
        deployer._helm_install = Mock()  # type: ignore[method-assign]
        deployer._runtime_forwards = Mock(return_value=nullcontext())  # type: ignore[method-assign]
        deployer._wait_rpc = Mock()  # type: ignore[method-assign]
        deployer._wait_tempo_faucet_window = Mock()  # type: ignore[method-assign]
        deployer._run_live_rust_test = Mock(return_value="evidence")  # type: ignore[method-assign]

        output = deployer.mpp_tempo()

        self.assertEqual(output, "evidence")
        deployer._helm_install.assert_called_once_with(  # type: ignore[attr-defined]
            "tempo",
            Path("/charts/tempo"),
            PAYMENT_NAMESPACE,
        )
        deployer._runtime_forwards.assert_called_once_with(("tempo",))  # type: ignore[attr-defined]
        self.assertEqual(deployer.environment["TEMPO_RPC_URL"], "http://127.0.0.1:28546/")


if __name__ == "__main__":
    unittest.main()
