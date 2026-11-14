import json
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest.mock import Mock, patch

from util.docker import E2EDockerCleaner, E2EDockerDeployer
from util.images import E2EImagesDefinitions as Images
from util.kubernetes import E2EKubernetesCleaner, E2EKubernetesDeployer
from util.process import E2EProcessHelper as Process
from util.report import E2EReportHelper
from util.sqlite import E2ESQLiteChecker, E2ESQLiteImporter


class RuntimeTest(unittest.TestCase):
    def test_image_network_and_host_matrix(self):
        for network in ("IN_CN_GFW", "NON_GFW"):
            for host in ("amd64", "arm64"):
                with (
                    self.subTest(network=network, host=host),
                    patch.object(Images, "host_architecture", return_value=host),
                ):
                    env = {"E2E_IMAGE_NETWORK": network}
                    Images.configure(env)
                    self.assertEqual(env["E2E_IMAGE_PLATFORM"], "linux/amd64")
                    expected = "linux/arm64" if host == "arm64" else "linux/amd64"
                    self.assertEqual(env["E2E_SOLANA_PLATFORM"], expected)
                    if host == "arm64":
                        self.assertEqual(env["E2E_SOLANA_IMAGE"], Images.SOLANA_ARM64)
        self.assertEqual(
            Images.network({"E2E_IMAGE_NETWORK": "IN_GFW"}), Images.IN_CN_GFW
        )

    def test_image_override(self):
        env = {"E2E_IMAGE_NETWORK": "NON_GFW", "E2E_POSTGRES_IMAGE": "local/test:1"}
        Images.configure(env)
        self.assertEqual(Images.reference("postgres", env), "local/test:1")

    def test_docker_explicit_context_does_not_fallback(self):
        with patch.object(
            Process,
            "execute",
            return_value=subprocess.CompletedProcess([], 1, "unavailable"),
        ) as run:
            with self.assertRaises(RuntimeError):
                E2EDockerDeployer.detect({"E2E_DOCKER_CONTEXT": "colima-test"})
            self.assertEqual(
                run.call_args.args[0][:3], ["docker", "--context", "colima-test"]
            )
            self.assertEqual(run.call_count, 1)

    def test_docker_discovers_local_engines(self):
        for context in ("native", "orbstack", "colima"):
            with self.subTest(context=context):

                def execute(command, *args):
                    selected = command[2] if "--context" in command else "native"
                    return subprocess.CompletedProcess(
                        command, int(selected != context), ""
                    )

                with (
                    patch("util.docker.sys.platform", "darwin"),
                    patch.object(Process, "execute", side_effect=execute),
                ):
                    actual = E2EDockerDeployer.detect({})
                    self.assertEqual(
                        actual,
                        ["docker"]
                        if context == "native"
                        else ["docker", "--context", context],
                    )

    def test_kubernetes_runtime_detection(self):
        for context, runtime in (
            ("colima", "docker"),
            ("colima-dev", "containerd"),
            ("orbstack", "containerd"),
            ("orbstack-dev", "containerd"),
            ("native", "containerd"),
            ("native", "docker"),
        ):
            for k3s in (True, False):
                with self.subTest(context=context, runtime=runtime, k3s=k3s):
                    nodes = {
                        "items": [
                            {
                                "metadata": {"name": "host"},
                                "status": {
                                    "nodeInfo": {
                                        "architecture": "amd64",
                                        "containerRuntimeVersion": runtime + "://1",
                                        "kubeletVersion": "v1.35.0+k3s1"
                                        if k3s
                                        else "v1.35.0",
                                        "machineID": "machine",
                                    }
                                },
                            }
                        ]
                    }

                    def run(command, *args):
                        if "current-context" in command:
                            return context
                        if "nodes" in command:
                            return json.dumps(nodes)
                        if "--format={{.ID}}" in command:
                            return "engine-id"
                        if "--format={{.Name}}" in command:
                            return "host"
                        return ""

                    with (
                        patch.object(Process, "run", side_effect=run),
                        patch.object(
                            Process,
                            "execute",
                            return_value=subprocess.CompletedProcess([], 0, ""),
                        ),
                        patch("util.kubernetes.sys.platform", "linux"),
                        patch(
                            "util.kubernetes.socket.gethostname", return_value="host"
                        ),
                        patch.object(Path, "is_file", return_value=True),
                        patch.object(Path, "read_text", return_value="machine"),
                        patch("util.kubernetes.shutil.which", return_value="/bin/k3s"),
                    ):
                        loader = E2EKubernetesDeployer.LocalImageLoader.detect(
                            ["kubectl"], {}
                        )
                    self.assertEqual(loader.platform, "linux/amd64")
                    if context == "orbstack" or runtime == "docker":
                        self.assertTrue(loader.shared_docker)
                    else:
                        self.assertIn("k3s" if k3s else "ctr", loader.ctr)
                        if context.startswith("colima"):
                            self.assertEqual(
                                loader.ctr[:4], ["colima", "-p", "dev", "ssh"]
                            )

    def test_emulation_mismatch_rejected(self):
        loader = E2EKubernetesDeployer.LocalImageLoader(
            "linux/arm64", ["docker"], [], [], {}
        )
        with self.assertRaises(RuntimeError):
            loader.validate_platform("linux/amd64", "app")
        loader.emulated_platforms = ("linux/amd64",)
        loader.validate_platform("linux/amd64", "app")

    def test_cleanup_namespace_scope_is_configurable(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "namespaces.yaml"
            path.write_text("kind: Namespace\nmetadata:\n  name: test-demo\n")
            cleaner = E2EKubernetesCleaner(
                10,
                namespace_manifest=path,
                namespace_prefix="test-",
                namespace_selector="suite=demo",
            )
            self.assertEqual(cleaner._declared_namespaces(), frozenset({"test-demo"}))
            path.write_text("kind: Namespace\nmetadata:\n  name: production\n")
            with self.assertRaises(RuntimeError):
                cleaner._declared_namespaces()

    def test_kubernetes_uses_the_callers_kubeconfig_for_detection_and_commands(self):
        with tempfile.TemporaryDirectory() as directory:
            config = (
                str(Path(directory) / "first") + ":" + str(Path(directory) / "second")
            )
            environment = {"E2E_IMAGE_NETWORK": "NON_GFW", "KUBECONFIG": config}
            with (
                patch.dict("os.environ", {"KUBECONFIG": "/another-cluster"}),
                patch.object(
                    E2EKubernetesDeployer.LocalImageLoader,
                    "detect",
                    return_value=Mock(),
                ) as detect,
                patch.object(Process, "run", return_value="pods") as run,
            ):
                deployer = E2EKubernetesDeployer(environment)
                self.assertEqual(deployer.kubectl("get", "pods"), "pods")
                self.assertEqual(detect.call_args.args[1]["KUBECONFIG"], config)
                self.assertEqual(run.call_args.args[2]["KUBECONFIG"], config)
                self.assertEqual(
                    E2EKubernetesDeployer.LocalImageLoader.kubeconfig({}),
                    str(Path.home() / ".kube/config"),
                )
                self.assertEqual(
                    E2EKubernetesDeployer.LocalImageLoader.kubeconfig(),
                    "/another-cluster",
                )

    def test_docker_cleanup_discovers_network_owners_without_service_names(self):
        cleaner = object.__new__(E2EDockerCleaner)
        cleaner.docker = ["docker"]
        cleaner.compose = ["docker", "compose", "-p", "test-suite"]
        cleaner.environment = {}
        inventory = [
            ("a" * 64, "database", "bridge"),
            ("b" * 64, "proxy", "container:" + "a" * 12),
            ("c" * 64, "worker", "container:/proxy"),
            ("d" * 64, "observer", "container:external-owner"),
        ]
        with (
            patch.object(cleaner, "_owned_container_inventory", return_value=inventory),
            patch.object(cleaner, "_owned_volumes", return_value=[]),
            patch.object(cleaner, "_owned_containers", return_value=[]),
            patch.object(
                Process, "execute", return_value=subprocess.CompletedProcess([], 0, "")
            ) as run,
        ):
            cleaner.cleanup()
        removals = [
            call.args[0][3:]
            for call in run.call_args_list
            if call.args[0][1:3] == ["rm", "--force"]
        ]
        self.assertEqual(removals, [["c" * 64, "d" * 64], ["b" * 64], ["a" * 64]])

    def test_sqlite_backup_includes_wal_and_checker_is_readonly(self):
        with tempfile.TemporaryDirectory() as directory:
            source, target = [
                Path(directory) / name for name in ("active.db", "snapshot.db")
            ]
            with sqlite3.connect(source) as live:
                live.execute("PRAGMA journal_mode=WAL")
                live.execute("CREATE TABLE evidence(value)")
                live.execute("INSERT INTO evidence VALUES (7)")
                live.commit()
                E2ESQLiteImporter.snapshot(source, target)
            with E2ESQLiteChecker.connect(target) as check:
                self.assertEqual(
                    E2ESQLiteChecker.one(check, "SELECT value FROM evidence"), (7,)
                )
                with self.assertRaises(sqlite3.OperationalError):
                    check.execute("DELETE FROM evidence")

    def test_archive_protects_evidence_and_incomplete_runs(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for run in ("20260101000000", "20260102000000", "20260103000000"):
                (root / run).mkdir()
                (root / run / "summary.md").write_text("complete")
            incomplete = root / "20250101000000"
            incomplete.mkdir()
            archived = E2EReportHelper.archive(
                root, keep=1, protected=("20260102000000",)
            )
            self.assertEqual([p.name for p in archived], ["20260101000000"])
            self.assertTrue(incomplete.is_dir())
            self.assertTrue((root / "20260102000000").is_dir())

    def test_background_process_is_closed_on_exception(self):
        import sys

        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            with self.assertRaisesRegex(RuntimeError, "stop verification"):
                with Process.background(
                    [sys.executable, "-c", "import time; time.sleep(60)"],
                    root,
                    root / "process.log",
                ) as process:
                    raise RuntimeError("stop verification")
            self.assertIsNotNone(process.poll())
            self.assertTrue((root / "process.log").is_file())

    def test_source_identity_covers_migrations_tests_and_browser_sources(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            names = ("migrations/001.sql", "tests/verify.py", "web/src/main.ts")
            for name in names:
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text("initial")

            def git(command, **kwargs):
                if "rev-parse" in command:
                    return "test-revision\n"
                for required in ("tests", "migrations", "web", "Cargo.lock"):
                    self.assertIn(required, command)
                return ("\0".join(names) + "\0").encode()

            with patch("subprocess.check_output", side_effect=git):
                before = E2EReportHelper.source_identity(root)
                self.assertEqual(before["scope_version"], "2")
                for name in names:
                    (root / name).write_text("changed")
                    self.assertNotEqual(E2EReportHelper.source_identity(root), before)
                    (root / name).write_text("initial")
