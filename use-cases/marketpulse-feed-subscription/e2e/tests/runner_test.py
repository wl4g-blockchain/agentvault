from __future__ import annotations

import io
import time
import unittest
from contextlib import redirect_stderr
from unittest.mock import Mock, patch

from runner import ProgressLogger, cleanup, parser, selected_scenarios


class RunnerTest(unittest.TestCase):
    def test_sdk_scope_defaults_to_simple(self) -> None:
        self.assertEqual(parser().parse_args(["-s", "matrix"]).e2e_sdk_scope, "simple")

    def test_sdk_scope_accepts_full(self) -> None:
        self.assertEqual(
            parser().parse_args(["-s", "matrix", "--e2e-sdk-scope", "full"]).e2e_sdk_scope,
            "full",
        )

    def test_cleanup_is_the_only_explicit_cleanup_option(self) -> None:
        self.assertTrue(parser().parse_args(["--cleanup"]).cleanup)
        self.assertEqual(parser().parse_args(["cleanup"]).command, "cleanup")
        with redirect_stderr(io.StringIO()):
            with self.assertRaises(SystemExit):
                parser().parse_args(["--cleanup-all"])

    @patch("runner.KubernetesE2ECleaner")
    @patch("runner.create_deployer")
    def test_cleanup_removes_all_workers_images_and_kubernetes(
        self,
        create: Mock,
        kubernetes_cleaner: Mock,
    ) -> None:
        deployers = [Mock(), Mock(), Mock()]
        create.side_effect = deployers

        cleanup(60, "docker", matrix_workers=1)

        self.assertEqual([call.args[0].worker_slot for call in create.call_args_list], [None, 0, 1])
        for deployer in deployers:
            deployer.cleanup.assert_called_once_with()
        deployers[0].cleanup_images.assert_called_once_with()
        deployers[1].cleanup_images.assert_not_called()
        deployers[2].cleanup_images.assert_not_called()
        kubernetes_cleaner.assert_called_once_with(60)
        kubernetes_cleaner.return_value.cleanup.assert_called_once_with()

    @patch("runner.KubernetesE2ECleaner")
    @patch("runner.create_deployer")
    def test_cleanup_after_run_preserves_images_and_kubernetes(
        self,
        create: Mock,
        kubernetes_cleaner: Mock,
    ) -> None:
        deployers = [Mock(), Mock()]
        create.side_effect = deployers

        cleanup(60, "docker", matrix_workers=1, comprehensive=False)

        self.assertEqual([call.args[0].worker_slot for call in create.call_args_list], [None, 0])
        for deployer in deployers:
            deployer.cleanup.assert_called_once_with()
            deployer.cleanup_images.assert_not_called()
        kubernetes_cleaner.assert_not_called()

    def test_advertises_explicit_mpp_tempo_gate(self) -> None:
        self.assertEqual(selected_scenarios(["mpp-tempo"])[0].report_name, "s25_mpp_tempo.md")

    def test_advertises_explicit_x402_solana_gate(self) -> None:
        self.assertEqual(selected_scenarios(["x402-solana"])[0].report_name, "s26_x402_solana.md")

    def test_progress_logger_reports_start_heartbeat_and_pass(self) -> None:
        with self.assertLogs("agentvault.e2e.runner", level="INFO") as captured:
            with ProgressLogger(heartbeat_seconds=0.01).step("scenario", scenario="structure"):
                time.sleep(0.03)

        output = "\n".join(captured.output)
        self.assertIn("START step=scenario scenario=structure", output)
        self.assertIn("RUNNING step=scenario scenario=structure", output)
        self.assertIn("PASS step=scenario scenario=structure", output)

    def test_progress_logger_reports_failure(self) -> None:
        with self.assertLogs("agentvault.e2e.runner", level="INFO") as captured:
            with self.assertRaisesRegex(RuntimeError, "broken"):
                with ProgressLogger().step("scenario", heartbeat=False, scenario="full"):
                    raise RuntimeError("broken")

        output = "\n".join(captured.output)
        self.assertIn("START step=scenario scenario=full", output)
        self.assertIn("FAIL step=scenario scenario=full", output)


if __name__ == "__main__":
    unittest.main()
