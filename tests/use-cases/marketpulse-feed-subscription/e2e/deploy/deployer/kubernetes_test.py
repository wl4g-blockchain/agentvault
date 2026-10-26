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


if __name__ == "__main__":
    unittest.main()
