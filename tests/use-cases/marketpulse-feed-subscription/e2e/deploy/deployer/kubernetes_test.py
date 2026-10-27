"""Kubernetes lifecycle and real SQLite evidence UTs; no chain or cluster is mocked as passed."""
from __future__ import annotations

import base64
import json
import io
import os
from pathlib import Path
import shutil
import sqlite3
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock, patch
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[6]
E2E = ROOT / "use-cases/marketpulse-feed-subscription/e2e"
sys.path.insert(0, str(E2E))

from common.context import VerificationContext
from deploy.deployer import kubernetes as backend


class KubernetesEvidenceTest(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.root = Path(self.temporary.name)
        self.context = VerificationContext("test-run", self.root / "reports", E2E / "deploy", "matrix", 1,
            "rust", "rust", "grpc", "kubernetes", 60, False, True, matrix_workers=1)

    def deployer(self):
        value = object.__new__(backend.KubernetesE2EDeployer)
        value.context = self.context
        value.images = dict(backend.LOCAL_IMAGES)
        value.environment = {}
        return value

    def test_solana_cli_and_keypair_use_actual_chart_deployment_name(self):
        value = self.deployer()
        value._kubectl = Mock(return_value="public-key")
        self.assertEqual(value._solana_command("solana", "cluster-version"), "public-key")
        self.assertIn("deployment/solana-validator", value._kubectl.call_args.args)
        value._kubectl = Mock(return_value=json.dumps([7] * 64))
        self.assertEqual(json.loads(value._solana_keypair_json("/tmp/test-keypair.json")), [7] * 64)
        self.assertIn("deployment/solana-validator", value._kubectl.call_args.args)
        value._kubectl.assert_called_once()

    def test_prebuilt_dirty_image_reaches_import_and_helm_without_runtime_rebuild(self):
        kubeconfig = self.root / "kubeconfig"
        kubeconfig.write_text("unit fixture")
        reference = "ghcr.io/example/agentvault:dirty-01234567"
        with patch.dict(os.environ, {"KUBECONFIG": str(kubeconfig), "E2E_AGENTVAULT_IMAGE": reference,
                "E2E_RUNNER_PREBUILT_AGENTVAULT": "1"}, clear=True), \
                patch.object(backend.shutil, "which", side_effect=lambda name: name), \
                patch.object(backend, "execute", return_value=subprocess.CompletedProcess([], 0, "")):
            value = backend.KubernetesE2EDeployer(self.context)
        self.assertEqual(value.environment["E2E_AGENTVAULT_IMAGE"], reference)
        value._run = Mock(return_value="")
        value._target_budget = Mock()
        value._ensure_cluster_image = Mock()
        value._build_images("rust", "rust")
        arguments = value._run.call_args.args[0]
        self.assertNotIn("agentvault", arguments[arguments.index("build") + 1:])
        self.assertIn(reference, [call.args[0] for call in value._ensure_cluster_image.call_args_list])
        value._helm_install = Mock()
        value._install_agentvault()
        self.assertEqual(value._helm_install.call_args.kwargs["set_strings"], {
            "agentvault.backend.image.repository": "ghcr.io/example/agentvault",
            "agentvault.backend.image.tag": "dirty-01234567", "agentvault.backend.image.pullPolicy": "Never",
        })

    def test_same_tag_with_different_config_digest_is_reimported_and_verified(self):
        value = self.deployer()
        value.docker = ["docker"]
        reference = "localhost/marketpulse-rust-e2e:dirty-01234567"
        expected = "f" * 64
        value._cluster_image_id = Mock(side_effect=["a" * 64, expected])
        exporter = SimpleNamespace(stdout=io.BytesIO(), returncode=0, communicate=Mock(return_value=(b"", b"")))
        importer = SimpleNamespace(returncode=0, communicate=Mock(return_value=(b"imported", b"")))
        with patch.object(backend, "execute", return_value=subprocess.CompletedProcess([], 0, "sha256:" + expected)), \
                patch.object(backend.subprocess, "Popen", side_effect=[exporter, importer]) as processes:
            value._ensure_cluster_image(reference)
        self.assertEqual(processes.call_args_list[0].args[0], ["docker", "save", reference])
        self.assertIn("import", processes.call_args_list[1].args[0])
        self.assertEqual(value._cluster_image_id.call_count, 2)

    def test_unverifiable_local_image_and_remote_named_cleanup_fail_closed(self):
        value = self.deployer()
        value.docker = ["docker"]
        value._cluster_images = Mock(return_value=set())
        value.images = {"agentvault": "ghcr.io/example/agentvault:dirty-01234567"}
        with patch.object(backend, "execute", return_value=subprocess.CompletedProcess([], 1, "missing")) as command:
            with self.assertRaisesRegex(RuntimeError, "absent from the local"):
                value._ensure_cluster_image(value.images["agentvault"])
            command.reset_mock()
            value.cleanup_images()
            command.assert_not_called()

    def test_two_payer_jobs_share_one_case_claim_not_owned_by_either_job(self):
        value = self.deployer()
        value._apply_document = Mock()
        value._apply_business_claims("rust", "go")
        claims = [call.args[0] for call in value._apply_document.call_args_list]
        self.assertEqual(len(claims), 2)
        self.assertEqual({item["metadata"]["namespace"] for item in claims}, {backend.PAYER_NAMESPACE, backend.PAYEE_NAMESPACE})
        self.assertTrue(all("ownerReferences" not in item["metadata"] for item in claims))
        self.assertTrue(all(item["metadata"]["labels"][backend.BUSINESS_EVIDENCE_LABEL] == "true" for item in claims))
        value._ensure_cluster_image = Mock()
        value._uninstall_release = Mock()
        value._kubectl = Mock(return_value="")
        value._helm_install = Mock()
        value._start_payer_job("rust", "go", failure_once=True)
        value._start_payer_job("rust", "go", failure_once=False)
        first, second = [call.kwargs["set_strings"] for call in value._helm_install.call_args_list]
        self.assertEqual(first["persistence.existingClaim"], second["persistence.existingClaim"])
        self.assertEqual(first["persistence.existingClaim"], "business-payer-rust")
        self.assertIn("env.QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT", first)
        self.assertNotIn("env.QUANTSTREAM_E2E_FAIL_AFTER_SETTLEMENT", second)
        self.assertTrue(all(call.args[1] == "job" for call in value._kubectl.call_args_list))

    def test_cleanup_is_label_scoped_and_rejects_foreign_namespace_or_retained_claim(self):
        cleaner = object.__new__(backend.KubernetesE2ECleaner)
        cleaner._kubectl_command = Mock(side_effect=[subprocess.CompletedProcess([], 0, "deleted"), subprocess.CompletedProcess([], 0, "")])
        cleaner._remove_business_evidence((backend.PAYER_NAMESPACE,), "persistentvolumeclaims")
        for call in cleaner._kubectl_command.call_args_list:
            arguments = call.args[0]
            self.assertEqual(arguments[arguments.index("--namespace") + 1], backend.PAYER_NAMESPACE)
            self.assertEqual(arguments[arguments.index("--selector") + 1], backend.BUSINESS_EVIDENCE_SELECTOR)
        with self.assertRaises(ValueError):
            cleaner._remove_business_evidence(("production",), "persistentvolumeclaims")
        cleaner._kubectl_command = Mock(side_effect=[subprocess.CompletedProcess([], 0, "deleted"), subprocess.CompletedProcess([], 0, "persistentvolumeclaim/stuck")])
        with self.assertRaisesRegex(RuntimeError, "remains"):
            cleaner._remove_business_evidence((backend.PAYER_NAMESPACE,), "persistentvolumeclaims")

    def test_manual_controller_cleanup_is_label_and_namespace_scoped(self):
        cleaner = object.__new__(backend.KubernetesE2ECleaner)
        cleaner._kubectl_command = Mock(side_effect=[subprocess.CompletedProcess([], 0, "deleted"),
                                                     subprocess.CompletedProcess([], 0, "")])
        cleaner._remove_manual_workloads((backend.PAYEE_NAMESPACE,))
        for call in cleaner._kubectl_command.call_args_list:
            args = call.args[0]
            self.assertEqual(args[1], "deployment,service,cronjob")
            self.assertEqual(args[args.index("--namespace")+1], backend.PAYEE_NAMESPACE)
            self.assertEqual(args[args.index("--selector")+1], "agentvault.io/manual-acceptance=true")
            self.assertNotIn("persistentvolumeclaims", args)
            self.assertNotIn("--all", args)
        with self.assertRaises(ValueError): cleaner._remove_manual_workloads(("production",))
        cleaner._kubectl_command = Mock(side_effect=[subprocess.CompletedProcess([], 0, "deleted"),
                                                     subprocess.CompletedProcess([], 0, "deployment/stuck")])
        with self.assertRaisesRegex(RuntimeError,"controllers remain"):
            cleaner._remove_manual_workloads((backend.PAYEE_NAMESPACE,))

    def test_cli_cleanup_removes_manual_controllers_before_waiting_for_pods(self):
        cleaner = object.__new__(backend.KubernetesE2ECleaner)
        cleaner._kubeconfig = self.root / "kubeconfig"
        cleaner._kubeconfig.write_text("unit")
        cleaner._kubectl = "kubectl"; cleaner._helm = "helm"
        cleaner._target_namespaces = Mock(return_value=(backend.PAYEE_NAMESPACE,))
        order = []
        cleaner._remove_manual_workloads = Mock(side_effect=lambda _: order.append("controllers") or [])
        cleaner._releases = Mock(return_value=())
        cleaner._remove_business_evidence = Mock(side_effect=lambda _,kind: order.append(kind) or [])
        cleaner._wait_for_pods = Mock(side_effect=lambda _: order.append("pods"))
        cleaner._recover_remaining_pods = Mock(return_value=[])
        cleaner._pods = Mock(return_value=())
        cleaner._release_manual_image_pins = Mock(side_effect=lambda: order.append("pins") or [])
        cleaner.cleanup()
        self.assertEqual(order,["controllers","jobs","pods","persistentvolumeclaims","pins"])
        self.assertEqual(backend.SERVICE_NAMES["solana"],(backend.PAYMENT_NAMESPACE,"solana-validator",8899))

    def test_snapshot_reads_committed_live_wal_and_produces_repeatably_readable_standalone_database(self):
        source = self.root / "live.sqlite3"
        connection = sqlite3.connect(source)
        self.addCleanup(connection.close)
        connection.execute("PRAGMA journal_mode=WAL")
        connection.execute("PRAGMA wal_autocheckpoint=0")
        connection.execute("CREATE TABLE evidence (value TEXT)")
        connection.execute("INSERT INTO evidence VALUES ('committed only in WAL')")
        connection.commit()
        self.assertTrue(Path(f"{source}-wal").is_file())
        value, jobs = self.snapshot_deployer(source)
        value._snapshot_business_database("payer", "rust", self.root / "snapshot")
        job = jobs[0]
        pod = job["spec"]["template"]["spec"]
        self.assertTrue(pod["volumes"][0]["persistentVolumeClaim"]["readOnly"])
        self.assertTrue(pod["containers"][0]["volumeMounts"][0]["readOnly"])
        self.assertFalse(pod["automountServiceAccountToken"])
        target = self.root / "snapshot/quantstream-rust/ingestion.sqlite3"
        for _ in range(2):
            with sqlite3.connect(f"file:{target}?mode=ro", uri=True) as copied:
                self.assertEqual(copied.execute("SELECT value FROM evidence").fetchone()[0], "committed only in WAL")
                self.assertEqual(copied.execute("PRAGMA journal_mode").fetchone()[0], "delete")
            self.assertFalse(Path(f"{target}-wal").exists())
            self.assertFalse(Path(f"{target}-shm").exists())

    def test_missing_business_database_fails_closed(self):
        value, _ = self.snapshot_deployer(self.root / "missing.sqlite3")
        with self.assertRaises(RuntimeError):
            value._snapshot_business_database("payer", "rust", self.root / "missing-result")
        self.assertFalse((self.root / "missing-result/quantstream-rust/ingestion.sqlite3").exists())

    def test_non_sqlite_snapshot_fails_closed(self):
        value = self.deployer()
        value._apply_document = Mock()
        value._kubectl = Mock(side_effect=["", json.dumps({"sqlite_snapshot": base64.b64encode(b"not SQLite").decode()})])
        with self.assertRaisesRegex(RuntimeError, "not SQLite"):
            value._snapshot_business_database("payer", "rust", self.root / "invalid-result")

    def snapshot_deployer(self, source):
        value = self.deployer()
        jobs, outputs = [], []
        value._apply_document = jobs.append
        def kubectl(*arguments, **_kwargs):
            if arguments[0] == "wait":
                command = jobs[-1]["spec"]["template"]["spec"]["containers"][0]["command"]
                target = self.root / f"unit-backup-{len(jobs)}.sqlite3"
                result = subprocess.run([sys.executable, "-c", command[2], str(source), str(target)], text=True, capture_output=True)
                if result.returncode:
                    raise RuntimeError(result.stderr)
                outputs.append(result.stdout)
                return ""
            return outputs[-1]
        value._kubectl = kubectl
        return value, jobs

    def business_databases(self, invalid_recovery=False):
        directory = self.root / "business"
        directory.mkdir()
        payer, payee = directory / "payer.sqlite3", directory / "payee.sqlite3"
        with sqlite3.connect(payer) as connection:
            connection.execute("CREATE TABLE ingestion_run(run_id,free_response_json,paid_response_json,payment_intent_id,completed_at)")
            for index in range(2):
                free = {"news": ["fixture"], "paid": False, "intentId": None}
                paid = {"news": ["fixture"], "paid": True, "intentId": f"in-{index}"}
                connection.execute("INSERT INTO ingestion_run VALUES (?,?,?,?,?)", (f"run-{index}", json.dumps(free), json.dumps(paid), f"out-{index}", index))
        with sqlite3.connect(payee) as connection:
            connection.execute("CREATE TABLE mcp_delivery(run_id,intent_id,status,response_json,failure_injections,updated_at)")
            for index in range(2):
                response = {"result": {"structuredContent": {"intentId": f"in-{index}"}}}
                injections = 0 if invalid_recovery else int(index == 0)
                connection.execute("INSERT INTO mcp_delivery VALUES (?,?,?,?,?,?)", (f"run-{index}", f"in-{index}", "delivered", json.dumps(response), injections, index))
        return payer, payee

    def test_audit_derives_two_runs_and_recovery_from_real_sqlite_rows(self):
        sources = self.business_databases()
        value = self.deployer()
        def snapshot(role, language, directory):
            component, filename, source = ("quantstream", "ingestion.sqlite3", sources[0]) if role == "payer" else ("marketpulse", "marketpulse.sqlite3", sources[1])
            target = directory / f"{component}-{language}" / filename
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(source, target)
        value._snapshot_business_database = snapshot
        result = value._audit("rust", "go")
        self.assertEqual(result["run_ids"], ["run-0", "run-1"])
        self.assertEqual(result["outbound_intent_ids"], ["out-0", "out-1"])
        self.assertEqual(result["inbound_intent_ids"], ["in-0", "in-1"])
        with sqlite3.connect(sources[1]) as connection:
            connection.execute("UPDATE mcp_delivery SET failure_injections=0")
        with self.assertRaisesRegex(RuntimeError, "post-settlement recovery"):
            value._audit("rust", "go")


class JobWaitTest(unittest.TestCase):
    class Clock:
        def __init__(self): self.now = 0.0
        def monotonic(self): return self.now
        def sleep(self, seconds): self.now += seconds

    def setUp(self):
        self.clock = self.Clock()
        self.addCleanup(patch.stopall)
        patch.object(backend.time, "monotonic", self.clock.monotonic).start()
        self.sleep = patch.object(backend.time, "sleep", side_effect=self.clock.sleep).start()
        self.port = object.__new__(backend.KubernetesE2EDeployer)
        self.port.environment = {}
        self.port._kubectl_executable = "kubectl"
        self.port._privileged_command = lambda *args: list(args)
        self.port._job_logs = Mock(return_value="payer log evidence")

    def response(self, status):
        return subprocess.CompletedProcess([], 0, json.dumps({"status": status,
            "spec": {"env": "do not include the full Job spec in timeout diagnostics"}}))

    def test_completed_job_returns_logs_without_sleeping_or_second_get(self):
        status = {"succeeded": 1, "conditions": [{"type": "Complete", "status": "True"}]}
        with patch.object(backend, "execute", return_value=self.response(status)) as command:
            self.assertEqual(self.port._wait_job("completed", 960), "payer log evidence")
        command.assert_called_once()
        self.assertIn("get", command.call_args.args[0])
        self.assertNotIn("wait", command.call_args.args[0])
        self.sleep.assert_not_called()
        self.port._job_logs.assert_called_once_with("completed")

    def test_failed_condition_returns_diagnostics_immediately_without_second_get(self):
        status = {"failed": 1, "conditions": [{"type": "Failed", "status": "True",
                   "reason": "BackoffLimitExceeded", "message": "payer exited with an error"}]}
        with patch.object(backend, "execute", return_value=self.response(status)) as command:
            with self.assertRaisesRegex(RuntimeError, "BackoffLimitExceeded.*payer log evidence"):
                self.port._wait_job("failed", 960)
        command.assert_called_once()
        self.sleep.assert_not_called()
        self.port._job_logs.assert_called_once_with("failed")
        self.assertEqual(self.clock.now, 0)

    def test_failed_pod_count_and_false_conditions_allow_retry_then_completion(self):
        pending = {"failed": 1, "active": 1, "conditions": [{"type": "Failed", "status": "False"}]}
        complete = {"failed": 1, "succeeded": 1, "conditions": [{"type": "Complete", "status": "True"}]}
        with patch.object(backend, "execute", side_effect=[self.response(pending), self.response(complete)]) as command:
            self.assertEqual(self.port._wait_job("retrying", 10), "payer log evidence")
        self.assertEqual(command.call_count, 2)
        self.assertEqual(self.clock.now, 2)
        self.port._job_logs.assert_called_once_with("retrying")

    def test_timeout_retains_latest_status_and_logs_with_bounded_polling(self):
        with patch.object(backend, "execute", return_value=self.response({"active": 1})) as command:
            with self.assertRaises(RuntimeError) as caught:
                self.port._wait_job("pending", 3)
        message = str(caught.exception)
        self.assertIn('last_status={"active": 1}', message)
        self.assertIn("payer log evidence", message)
        self.assertNotIn("full Job spec", message)
        self.assertEqual(self.clock.now, 3)
        self.assertEqual(command.call_count, 2)
        self.assertEqual([call.args[3] for call in command.call_args_list], [3, 1])
        self.port._job_logs.assert_called_once_with("pending")

    def test_transient_api_error_is_retried_within_deadline(self):
        error = subprocess.CompletedProcess([], 1, "API temporarily unavailable")
        complete = self.response({"conditions": [{"type": "Complete", "status": "True"}]})
        with patch.object(backend, "execute", side_effect=[error, complete]) as command:
            self.assertEqual(self.port._wait_job("recovering-api", 10), "payer log evidence")
        self.assertEqual(command.call_count, 2)
        self.assertEqual(self.clock.now, 2)

    def test_failed_condition_is_preserved_even_when_log_collection_fails(self):
        status = {"conditions": [{"type": "Failed", "status": "True", "reason": "DeadlineExceeded"}]}
        self.port._job_logs.side_effect = RuntimeError("log request failed")
        with patch.object(backend, "execute", return_value=self.response(status)):
            with self.assertRaisesRegex(RuntimeError, "DeadlineExceeded.*logs unavailable"):
                self.port._wait_job("failed", 960)
        self.sleep.assert_not_called()

    def test_terminal_helper_does_not_confuse_retrying_pods_with_final_failure(self):
        with patch.object(backend, "execute", return_value=self.response({"failed": 2, "active": 1})) as command:
            self.assertIsNone(self.port._job_terminal_evidence("retrying"))
        command.assert_called_once()
        self.port._job_logs.assert_not_called()

    def test_nonpositive_or_unbounded_timeout_is_rejected_without_api_requests(self):
        with patch.object(backend, "execute") as command:
            for timeout in (0, -1, float("inf"), float("nan")):
                with self.assertRaises(ValueError): self.port._wait_job("invalid", timeout)
        command.assert_not_called()


class KubernetesManualPinCleanupTest(unittest.TestCase):
    def cleaner(self, directory):
        cleaner=object.__new__(backend.KubernetesE2ECleaner)
        cleaner._kubeconfig=Path(directory)/"kubeconfig"
        cleaner._kubeconfig.write_text("unit")
        cleaner._kubectl="kubectl";cleaner._helm="helm";cleaner._command_timeout=5
        cleaner._target_namespaces=Mock(return_value=())
        return cleaner

    def test_lost_state_and_absent_namespaces_still_release_only_owned_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            cleaner=self.cleaner(directory)
            self.assertFalse((Path(directory)/"manual-public.json").exists())
            owned="ghcr.io/example/custom-payer:dirty"
            operator="registry.example/operator:pinned"
            labels={owned:{backend.MANUAL_IMAGE_PIN:"pinned",backend.MANUAL_IMAGE_PIN_OWNER:"true"},
                    operator:{backend.MANUAL_IMAGE_PIN:"pinned","operator":"true"}}
            def command(arguments,*args):
                self.assertEqual(arguments[:7],["sudo","-n","k3s","ctr","-n","k8s.io","images"])
                if arguments[7]=="list":
                    self.assertEqual(arguments[8:],["--quiet",'labels."io.agentvault.manual-image-pin"=="true"'])
                    selected=[ref for ref,meta in labels.items() if meta.get(backend.MANUAL_IMAGE_PIN_OWNER)=="true"]
                    return subprocess.CompletedProcess(arguments,0,"\n".join(selected))
                self.assertEqual(arguments[7:],["label",owned,"io.cri-containerd.pinned=","io.agentvault.manual-image-pin="])
                labels[owned].pop(backend.MANUAL_IMAGE_PIN)
                labels[owned].pop(backend.MANUAL_IMAGE_PIN_OWNER)
                return subprocess.CompletedProcess(arguments,0,"")
            with patch.object(backend.shutil,"which",return_value="k3s"), \
                 patch.object(backend,"execute",side_effect=command) as commands:
                result=cleaner.cleanup()
            self.assertEqual(commands.call_count,2)
            self.assertNotIn(backend.MANUAL_IMAGE_PIN,labels[owned])
            self.assertEqual(labels[operator][backend.MANUAL_IMAGE_PIN],"pinned")
            self.assertIn(owned,result)
            self.assertTrue(all("--replace-all" not in call.args[0] and "remove" not in call.args[0]
                                for call in commands.call_args_list))

    def test_cleanup_without_manual_marker_never_changes_operator_pin(self):
        with tempfile.TemporaryDirectory() as directory:
            cleaner=self.cleaner(directory)
            with patch.object(backend.shutil,"which",return_value="k3s"), \
                 patch.object(backend,"execute",return_value=subprocess.CompletedProcess([],0,"")) as command:
                cleaner.cleanup()
            command.assert_called_once()
            self.assertEqual(command.call_args.args[0][-1],'labels."io.agentvault.manual-image-pin"=="true"')

    def test_non_k3s_cleanup_has_no_local_image_side_effect(self):
        with tempfile.TemporaryDirectory() as directory:
            cleaner=self.cleaner(directory)
            with patch.object(backend.shutil,"which",return_value=None), patch.object(backend,"execute") as command:
                cleaner.cleanup()
            command.assert_not_called()


if __name__ == "__main__":
    unittest.main()
