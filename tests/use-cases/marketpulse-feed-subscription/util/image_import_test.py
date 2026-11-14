import hashlib
import io
import json
import re
import subprocess
import tarfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

from util.constant import E2EOtherDefinitions as Project
from util.kubernetes import E2EKubernetesDeployer as Kubernetes
from util.process import E2EProcessHelper as Process


class ImageImportTest(unittest.TestCase):
    CONFIG = b'{"architecture":"amd64","os":"linux"}'
    MANIFEST_DIGEST = "sha256:" + "b" * 64

    @classmethod
    def archive(cls, stream, tags):
        with tarfile.open(fileobj=stream, mode="w") as archive:
            files = {
                "manifest.json": json.dumps(
                    [{"Config": "config.json", "RepoTags": tags, "Layers": []}]
                ).encode(),
                "index.json": json.dumps(
                    {"manifests": [{"digest": cls.MANIFEST_DIGEST}]}
                ).encode(),
                "config.json": cls.CONFIG,
            }
            for name, payload in files.items():
                info = tarfile.TarInfo(name)
                info.size = len(payload)
                archive.addfile(info, io.BytesIO(payload))

    def runtime(self):
        runtime = object.__new__(Kubernetes)
        runtime.environment = {"E2E_IMAGE_NETWORK": "NON_GFW"}
        runtime.context = SimpleNamespace(timeout_seconds=30)
        runtime.docker = ["docker"]
        runtime._image_loader = Kubernetes.LocalImageLoader(
            "linux/amd64",
            ["docker"],
            ["k3s", "ctr", "-n", "k8s.io"],
            ["k3s", "crictl"],
            runtime.environment,
        )
        return runtime

    def run_import(
        self,
        reference,
        tags,
        imported_id=None,
        legacy_error=None,
        platform="linux/amd64",
    ):
        runtime = self.runtime()
        expected = hashlib.sha256(self.CONFIG).hexdigest()
        store = {}
        runtime._cluster_image_id = lambda ref: store.get(ref)

        def command(args, **kwargs):
            if "save" in args:
                self.archive(kwargs["stdout"], tags)
            else:
                stream = kwargs["stdin"]
                stream.seek(0)
                with tarfile.open(fileobj=stream) as archive:
                    manifests = json.load(archive.extractfile("manifest.json"))
                    index = json.load(archive.extractfile("index.json"))
                for tag in manifests[0]["RepoTags"] or []:
                    store[tag] = imported_id or expected
                # Model ctr's reference creation, rather than assuming an
                # import return code makes anonymous images usable by CRI.
                if "--digests" in args:
                    base = (
                        args[args.index("--base-name") + 1]
                        if "--base-name" in args
                        else "import-date"
                    )
                    store[base + "@" + index["manifests"][0]["digest"]] = (
                        imported_id or expected
                    )
            return subprocess.CompletedProcess(args, 0, "imported", "")

        def inspect(args, *unused):
            if legacy_error and "--platform" in args:
                return subprocess.CompletedProcess(args, 1, legacy_error)
            return subprocess.CompletedProcess(
                args, 0, "sha256:" + "a" * 64 + " " + platform + "\n"
            )

        with (
            patch.object(Process, "execute", side_effect=inspect),
            patch("util.kubernetes.subprocess.run", side_effect=command),
        ):
            runtime._ensure_cluster_image(reference)
        return store

    def test_anonymous_saved_digest_is_imported_with_its_exact_repository_name(self):
        reference = "docker.io/pgvector/pgvector@" + self.MANIFEST_DIGEST
        store = self.run_import(reference, None)
        self.assertEqual(store[reference], hashlib.sha256(self.CONFIG).hexdigest())

    def test_named_local_application_import_retains_the_tag(self):
        reference = "localhost/agentvault-e2e:local"
        self.assertIn(reference, self.run_import(reference, [reference]))

    def test_import_still_rejects_an_image_with_the_wrong_config_digest(self):
        reference = "docker.io/pgvector/pgvector@" + self.MANIFEST_DIGEST
        for error in (None, "unknown flag: --platform"):
            with (
                self.subTest(legacy_error=error),
                self.assertRaisesRegex(
                    RuntimeError, "does not match the source config digest"
                ),
            ):
                self.run_import(reference, None, "c" * 64, legacy_error=error)

    def test_legacy_docker_inspection_imports_tagged_and_digest_images(self):
        for error in (
            "unknown flag: --platform\nSee 'docker image inspect --help'.",
            '"--platform" requires API version 1.49, but the Docker daemon API version is 1.48',
        ):
            for reference, tags in (
                (
                    "ghcr.io/example/agentvault:dirty-12345678",
                    ["ghcr.io/example/agentvault:dirty-12345678"],
                ),
                ("docker.io/pgvector/pgvector@" + self.MANIFEST_DIGEST, None),
            ):
                with self.subTest(error=error, reference=reference):
                    store = self.run_import(reference, tags, legacy_error=error)
                    self.assertEqual(
                        store[reference], hashlib.sha256(self.CONFIG).hexdigest()
                    )

    def test_legacy_inspection_still_rejects_a_platform_mismatch(self):
        with self.assertRaisesRegex(RuntimeError, "image platform mismatch"):
            self.run_import(
                "localhost/agentvault-e2e:local",
                [],
                legacy_error="unknown flag: --platform",
                platform="linux/arm64",
            )

    def test_inspection_failure_preserves_dockers_error_without_retrying(self):
        for error in (
            "Error response from daemon: No such image: missing:latest",
            "permission denied while trying to connect to the Docker daemon socket",
            "Error response from daemon: image does not provide the specified platform (linux/amd64)",
        ):
            with (
                self.subTest(error=error),
                patch.object(
                    Process,
                    "execute",
                    return_value=subprocess.CompletedProcess([], 1, error),
                ) as inspect,
            ):
                with self.assertRaisesRegex(
                    RuntimeError, "could not inspect"
                ) as raised:
                    self.runtime()._ensure_cluster_image("missing:latest")
                self.assertIn(error, str(raised.exception))
                self.assertEqual(inspect.call_count, 1)

    def test_legacy_inspection_reuses_an_existing_remote_deployment_image(self):
        reference = "docker.io/pgvector/pgvector:pg16"

        def inspect(args, *unused):
            if "--platform" in args:
                return subprocess.CompletedProcess(args, 1, "unknown flag: --platform")
            payload = (
                "linux/amd64\n"
                if "--format={{.Os}}/{{.Architecture}}" in args
                else json.dumps({"Os": "linux", "Architecture": "amd64"})
            )
            return subprocess.CompletedProcess(args, 0, payload)

        with (
            patch.object(Process, "execute", side_effect=inspect),
            patch.object(Process, "run") as run,
        ):
            selected = self.runtime()._image_loader.deployment_image(reference, 30)
        self.assertEqual(selected, reference)
        run.assert_not_called()

    def test_modern_inspection_pins_the_selected_remote_manifest(self):
        reference = "docker.io/pgvector/pgvector:pg16"

        def inspect(args, *unused):
            self.assertIn("--platform", args)
            payload = (
                "linux/amd64\n"
                if "--format={{.Os}}/{{.Architecture}}" in args
                else json.dumps(
                    {
                        "Descriptor": {
                            "mediaType": "application/vnd.oci.image.manifest.v1+json",
                            "digest": self.MANIFEST_DIGEST,
                        }
                    }
                )
            )
            return subprocess.CompletedProcess(args, 0, payload)

        with (
            patch.object(Process, "execute", side_effect=inspect),
            patch.object(Process, "run") as run,
        ):
            selected = self.runtime()._image_loader.deployment_image(reference, 30)
        self.assertEqual(
            selected, "docker.io/pgvector/pgvector@" + self.MANIFEST_DIGEST
        )
        run.assert_not_called()

    def test_every_shared_cargo_cache_mount_locks_its_writers(self):
        dockerfiles = [
            Project.REPOSITORY_ROOT / "deploy/docker/Dockerfile",
            *Project.DEPLOY_ROOT.glob("*/Dockerfile"),
        ]
        checked = 0
        for path in dockerfiles:
            if path.is_symlink():
                continue
            for options in re.findall(r"--mount=([^\s]+)", path.read_text()):
                if "type=cache" in options and "target=/usr/local/cargo/" in options:
                    with self.subTest(path=path, mount=options):
                        self.assertIn("sharing=locked", options.split(","))
                    checked += 1
        self.assertGreater(checked, 0)
