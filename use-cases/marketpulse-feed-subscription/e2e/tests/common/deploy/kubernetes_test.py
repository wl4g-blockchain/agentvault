from __future__ import annotations

import json
import subprocess
import unittest
from pathlib import Path
from unittest.mock import Mock

from common.deploy.kubernetes import HelmRelease, KubernetesE2ECleaner


def completed(returncode: int = 0, output: str = "") -> subprocess.CompletedProcess[str]:
    return subprocess.CompletedProcess([], returncode, output)


class KubernetesE2ECleanerTest(unittest.TestCase):
    def cleaner(self) -> KubernetesE2ECleaner:
        return KubernetesE2ECleaner(
            60,
            kubeconfig=Path(__file__),
            kubectl="/test/kubectl",
            helm="/test/helm",
        )

    def test_target_namespaces_require_both_manifest_membership_and_label(self) -> None:
        cleaner = self.cleaner()
        cleaner._kubectl_command = Mock(  # type: ignore[method-assign]
            return_value=completed(
                output=json.dumps(
                    {
                        "items": [
                            {"metadata": {"name": "e2e-agentvault-system"}},
                            {"metadata": {"name": "e2e-agentvault-not-declared"}},
                            {"metadata": {"name": "production"}},
                        ]
                    }
                )
            )
        )

        self.assertEqual(cleaner._target_namespaces(), ("e2e-agentvault-system",))

    def test_failed_uninstall_recovers_only_the_release_before_retry(self) -> None:
        cleaner = self.cleaner()
        release = HelmRelease("e2e-agentvault-system", "agentvault")
        cleaner._target_namespaces = Mock(  # type: ignore[method-assign]
            return_value=(release.namespace,)
        )
        cleaner._releases = Mock(  # type: ignore[method-assign]
            side_effect=[(release,), ()]
        )
        cleaner._helm_command = Mock(  # type: ignore[method-assign]
            side_effect=[completed(1, "timed out"), completed(0, "uninstalled")]
        )
        cleaner._recover_release = Mock(return_value=["recovered"])  # type: ignore[method-assign]
        cleaner._wait_for_pods = Mock()  # type: ignore[method-assign]
        cleaner._recover_remaining_pods = Mock(return_value=[])  # type: ignore[method-assign]
        cleaner._pods = Mock(return_value=())  # type: ignore[method-assign]

        output = cleaner.cleanup()

        cleaner._recover_release.assert_called_once_with(release)  # type: ignore[attr-defined]
        retry = cleaner._helm_command.call_args_list[1].args[0]  # type: ignore[attr-defined]
        self.assertIn("--no-hooks", retry)
        self.assertNotIn("--wait", retry)
        self.assertIn("recovered", output)

    def test_stranded_pod_finalizer_is_cleared_before_force_delete(self) -> None:
        cleaner = self.cleaner()
        cleaner._kubectl_command = Mock(  # type: ignore[method-assign]
            side_effect=[completed(output="patched"), completed(output="deleted")]
        )

        cleaner._clear_and_delete("pod/stuck", "e2e-agentvault-system")

        patch_args = cleaner._kubectl_command.call_args_list[0].args[0]  # type: ignore[attr-defined]
        delete_args = cleaner._kubectl_command.call_args_list[1].args[0]  # type: ignore[attr-defined]
        self.assertEqual(patch_args[:2], ["patch", "pod/stuck"])
        self.assertIn("--patch", patch_args)
        self.assertEqual(delete_args[:2], ["delete", "pod/stuck"])
        self.assertIn("--grace-period=0", delete_args)
        self.assertIn("--force", delete_args)

    def test_resource_inventory_ignores_kubectl_warning_lines(self) -> None:
        output = (
            "Warning: v1 Endpoints is deprecated; use EndpointSlice:\n"
            "endpoints/emqx\n"
            "deployment.apps/emqx\n"
        )

        self.assertEqual(
            KubernetesE2ECleaner._resource_references(output),
            ("deployment.apps/emqx", "endpoints/emqx"),
        )

    def test_empty_successful_pod_inventory_is_not_an_error(self) -> None:
        cleaner = self.cleaner()
        cleaner._kubectl_command = Mock(return_value=completed())  # type: ignore[method-assign]

        self.assertEqual(cleaner._pods(("e2e-agentvault-system",)), ())


if __name__ == "__main__":
    unittest.main()
